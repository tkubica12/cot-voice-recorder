"""Recording lifecycle service: create, get, chunk upload, and completion."""

from __future__ import annotations

import uuid

from ..domain import Chunk, ChunkState, Recording, RecordingState, can_transition
from ..errors import ConcurrencyConflict
from ..logging_config import get_logger
from ..models import ChunkAccepted, RecordingProgress
from ..models import Recording as RecordingModel
from ..problems import ChunkConflictError, CompleteConflictError, NotFoundError
from .context import ServiceContext, audio_path
from .finalize import (
    advance_active_state,
    arm_finalize,
    desired_active_state,
    received_indices,
    transcribed_indices,
)
from .messages import transcribe_message

logger = get_logger(__name__)


def to_api_recording(ctx: ServiceContext, recording: Recording) -> RecordingModel:
    chunks = ctx.chunks.list_for_recording(recording.recording_id)
    progress = RecordingProgress(
        expected_chunk_count=recording.expected_chunk_count,
        received_chunk_count=len(received_indices(chunks)),
        transcribed_chunk_count=len(transcribed_indices(chunks)),
    )
    return RecordingModel(
        recording_id=recording.recording_id,
        client_recording_id=recording.client_recording_id,
        state=recording.state.value,
        refine_model=recording.refine_model,
        language=recording.language,
        progress=progress,
        transcript_id=recording.transcript_id,
        failure_reason=(recording.failure_reason.value if recording.failure_reason else None),
        created_at=recording.created_at,
        updated_at=recording.updated_at,
    )


def create_recording(
    ctx: ServiceContext,
    *,
    client_recording_id: str,
    refine_model: str,
    language: str,
) -> tuple[Recording, bool]:
    """Idempotently create a recording keyed by ``client_recording_id``."""
    now = ctx.clock.now()
    candidate = Recording(
        recording_id=str(uuid.uuid4()),
        client_recording_id=client_recording_id,
        state=RecordingState.RECORDING,
        refine_model=refine_model,
        language=language,
        created_at=now,
        updated_at=now,
    )
    recording, created = ctx.recordings.create_if_absent(candidate)
    if created:
        logger.info(
            "recording_created",
            extra={"recording_id": recording.recording_id},
        )
    return recording, created


def get_recording(ctx: ServiceContext, recording_id: str) -> Recording:
    recording = ctx.recordings.get(recording_id)
    if recording is None:
        raise NotFoundError("Recording not found.")
    return recording


def upload_chunk(
    ctx: ServiceContext,
    *,
    recording_id: str,
    index: int,
    data: bytes,
    checksum: str,
    duration_ms: int | None,
    overlap_ms: int | None,
    started_at: object,
) -> tuple[ChunkAccepted, int]:
    """Store a chunk (idempotent) and enqueue transcription. Returns (model, status)."""
    recording = ctx.recordings.get(recording_id)
    if recording is None:
        raise NotFoundError("Recording not found.")

    existing = ctx.chunks.get(recording_id, index)
    if existing is not None:
        if existing.checksum == checksum:
            return _chunk_model(existing), 200
        raise ChunkConflictError(
            f"Chunk index {index} was already uploaded with a different checksum."
        )

    # Range check against a declared chunk_count (client declared fewer than it sent).
    if recording.expected_chunk_count is not None and index >= recording.expected_chunk_count:
        raise ChunkConflictError(
            f"Chunk index {index} is out of range; recording declared "
            f"chunk_count={recording.expected_chunk_count}."
        )

    now = ctx.clock.now()
    ctx.blobs.put(
        ctx.settings.audio_container,
        audio_path(recording_id, index),
        data,
        content_type="audio/wav",
    )
    chunk = Chunk(
        recording_id=recording_id,
        index=index,
        checksum=checksum,
        state=ChunkState.ACCEPTED,
        received_at=now,
        size_bytes=len(data),
        blob_path=audio_path(recording_id, index),
        duration_ms=duration_ms,
        overlap_ms=overlap_ms,
        started_at=started_at,  # type: ignore[arg-type]
    )
    stored, created = ctx.chunks.put_if_absent(chunk)
    if not created:
        # Lost a race with a concurrent identical upload.
        if stored.checksum == checksum:
            return _chunk_model(stored), 200
        raise ChunkConflictError(
            f"Chunk index {index} was already uploaded with a different checksum."
        )

    ctx.queue.send(transcribe_message(recording_id, index))
    advance_active_state(ctx, recording_id)
    logger.info(
        "chunk_accepted",
        extra={"recording_id": recording_id, "index": index, "size_bytes": len(data)},
    )
    return _chunk_model(stored), 202


def complete_recording(
    ctx: ServiceContext,
    *,
    recording_id: str,
    chunk_count: int,
) -> tuple[Recording, int]:
    """Declare the expected chunk count (idempotent). Returns (recording, status).

    A replay does not short-circuit: it re-arms finalize/watchdog work first, so a client
    retry repairs a recording whose finalize message was never successfully enqueued.
    """
    for _ in range(5):
        recording = ctx.recordings.get(recording_id)
        if recording is None:
            raise NotFoundError("Recording not found.")

        if recording.expected_chunk_count is not None:
            if recording.expected_chunk_count == chunk_count:
                # Idempotent replay: repair missing finalize work before answering.
                arm_finalize(ctx, recording_id)
                return ctx.recordings.get(recording_id) or recording, 200
            raise CompleteConflictError(
                f"Recording already completed with "
                f"chunk_count={recording.expected_chunk_count}; got {chunk_count}."
            )

        chunks = ctx.chunks.list_for_recording(recording_id)
        received = received_indices(chunks)
        # Client declared fewer chunks than it already uploaded.
        if received and max(received) >= chunk_count:
            raise CompleteConflictError(
                f"Declared chunk_count={chunk_count} is smaller than an already "
                f"uploaded chunk index {max(received)}."
            )

        provisional = recording.with_changes(expected_chunk_count=chunk_count)
        target = desired_active_state(provisional, chunks)
        if not can_transition(recording.state, target):
            target = recording.state
        updated = recording.with_changes(
            expected_chunk_count=chunk_count,
            complete_requested_at=ctx.clock.now(),
            state=target,
            updated_at=ctx.clock.now(),
        )
        try:
            saved = ctx.recordings.update(updated)
        except ConcurrencyConflict:
            continue
        logger.info(
            "recording_complete_requested",
            extra={"recording_id": recording_id, "chunk_count": chunk_count},
        )
        # All chunks may already be transcribed (e.g., short recordings); otherwise this
        # schedules the delayed watchdog that enforces the missing-chunk grace timeout.
        arm_finalize(ctx, recording_id)
        refreshed = ctx.recordings.get(recording_id) or saved
        return refreshed, 202
    raise CompleteConflictError("Could not complete recording due to concurrent updates.")


def _chunk_model(chunk: Chunk) -> ChunkAccepted:
    return ChunkAccepted(
        recording_id=chunk.recording_id,
        index=chunk.index,
        chunk_state=chunk.state.value,
        checksum=chunk.checksum,
        received_at=chunk.received_at,
    )

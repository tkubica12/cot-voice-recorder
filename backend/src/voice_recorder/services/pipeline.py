"""Worker-side transcription and finalization pipeline.

These functions are invoked by the queue worker. They are idempotent and raise
:class:`TransientError` for retryable failures (the worker backs off and retries) or
:class:`TerminalError` for non-retryable ones (the worker fails the recording).
"""

from __future__ import annotations

import uuid

from ..ai.protocols import TranscriptionHints
from ..domain import (
    ChunkState,
    FailureReason,
    RecordingState,
    Transcript,
    can_transition,
)
from ..errors import BlobNotFound, ConcurrencyConflict, TerminalError
from ..logging_config import get_logger
from ..models import TranscriptCompletedEvent
from ..prompts import build_transcription_prompt
from ..stitch import stitch_chunks
from ..text import make_preview
from .context import ServiceContext, audio_path, raw_path, transcript_path
from .finalize import all_expected_transcribed, try_enqueue_finalize

logger = get_logger(__name__)

_TRANSCRIPT_NS = uuid.uuid5(uuid.NAMESPACE_URL, "voice-recorder/transcript")
_EVENT_NS = uuid.uuid5(uuid.NAMESPACE_URL, "voice-recorder/event")


def _delete_audio(ctx: ServiceContext, recording_id: str, index: int) -> None:
    ctx.blobs.delete(ctx.settings.audio_container, audio_path(recording_id, index))


def transcribe_chunk(ctx: ServiceContext, recording_id: str, index: int) -> None:
    chunk = ctx.chunks.get(recording_id, index)
    if chunk is None:
        return
    if chunk.state == ChunkState.TRANSCRIBED:
        _delete_audio(ctx, recording_id, index)
        try_enqueue_finalize(ctx, recording_id)
        return
    if chunk.state == ChunkState.FAILED:
        return

    recording = ctx.recordings.get(recording_id)
    if recording is None or recording.state == RecordingState.FAILED:
        _delete_audio(ctx, recording_id, index)
        return

    try:
        audio = ctx.blobs.get(ctx.settings.audio_container, audio_path(recording_id, index))
    except BlobNotFound:
        latest = ctx.chunks.get(recording_id, index)
        if latest is not None and latest.state == ChunkState.TRANSCRIBED:
            return
        raise TerminalError("audio blob missing before transcription") from None

    text = ctx.transcriber.transcribe(
        audio,
        hints=TranscriptionHints(
            language=recording.language,
            prompt=build_transcription_prompt(ctx.settings.glossary_prompt()),
            phrases=ctx.settings.glossary_phrases(),
        ),
    )

    for _ in range(5):
        current = ctx.chunks.get(recording_id, index)
        if current is None or current.state == ChunkState.TRANSCRIBED:
            break
        try:
            ctx.chunks.update(current.with_changes(state=ChunkState.TRANSCRIBED, text=text))
            break
        except ConcurrencyConflict:
            continue

    _delete_audio(ctx, recording_id, index)
    logger.info(
        "chunk_transcribed",
        extra={"recording_id": recording_id, "index": index, "chars": len(text)},
    )
    try_enqueue_finalize(ctx, recording_id)


def finalize(ctx: ServiceContext, recording_id: str) -> None:
    recording = ctx.recordings.get(recording_id)
    if recording is None:
        return
    if recording.state == RecordingState.COMPLETED or recording.transcript_id is not None:
        return
    if recording.state == RecordingState.FAILED:
        return
    if recording.expected_chunk_count is None:
        # Completion was never requested; nothing to finalize yet.
        return

    chunks = ctx.chunks.list_for_recording(recording_id)
    if any(c.state == ChunkState.FAILED for c in chunks):
        fail_recording(ctx, recording_id, FailureReason.TRANSCRIPTION_FAILED)
        return
    if not all_expected_transcribed(recording, chunks):
        if _grace_exceeded(ctx, recording):
            fail_recording(ctx, recording_id, FailureReason.MISSING_CHUNKS)
            return
        # A late chunk is still being transcribed; retry finalize shortly.
        from ..errors import TransientError

        raise TransientError("waiting for all chunks to be transcribed")

    # Acquire the finalize lock by moving to REFINING (idempotent resume if already there).
    if recording.state != RecordingState.REFINING:
        if not can_transition(recording.state, RecordingState.REFINING):
            return
        try:
            recording = ctx.recordings.update(
                recording.with_changes(state=RecordingState.REFINING, updated_at=ctx.clock.now())
            )
        except ConcurrencyConflict:
            return  # Another finalizer won the race.

    expected = recording.expected_chunk_count or 0
    by_index = {c.index: c for c in chunks}
    ordered_texts = [(by_index[i].text or "") for i in range(expected)]
    raw_text = stitch_chunks(
        ordered_texts,
        min_overlap_chars=ctx.settings.overlap_min_chars,
        max_overlap_tokens=60,
    )
    ctx.blobs.put(
        ctx.settings.raw_container,
        raw_path(recording_id),
        raw_text.encode("utf-8"),
        content_type="text/plain; charset=utf-8",
    )

    refined = ctx.refiner.refine(raw_text, deployment=recording.refine_model)

    transcript_id = str(uuid.uuid5(_TRANSCRIPT_NS, recording_id))
    now = ctx.clock.now()
    from datetime import timedelta

    expires_at = now + timedelta(hours=ctx.settings.retention_hours)
    ctx.blobs.put(
        ctx.settings.transcript_container,
        transcript_path(transcript_id),
        refined.encode("utf-8"),
        content_type="text/plain; charset=utf-8",
    )
    preview = make_preview(refined, max_chars=ctx.settings.preview_max_chars)
    transcript = Transcript(
        transcript_id=transcript_id,
        recording_id=recording_id,
        preview=preview,
        language=recording.language,
        refine_model=recording.refine_model,
        completed_at=now,
        expires_at=expires_at,
        character_count=len(refined),
        body_path=transcript_path(transcript_id),
    )
    ctx.transcripts.create(transcript)

    for _ in range(5):
        current = ctx.recordings.get(recording_id)
        if current is None or current.state == RecordingState.COMPLETED:
            break
        try:
            ctx.recordings.update(
                current.with_changes(
                    state=RecordingState.COMPLETED,
                    transcript_id=transcript_id,
                    updated_at=ctx.clock.now(),
                )
            )
            break
        except ConcurrencyConflict:
            continue

    event = TranscriptCompletedEvent(
        event_id=str(uuid.uuid5(_EVENT_NS, recording_id)),
        transcript_id=transcript_id,
        recording_id=recording_id,
        completed_at=now,
        preview=preview,
    )
    ctx.realtime.notify_completed(ctx.user_id, event)
    logger.info(
        "recording_completed",
        extra={"recording_id": recording_id, "transcript_id": transcript_id},
    )


def _grace_exceeded(ctx: ServiceContext, recording: object) -> bool:
    from datetime import timedelta

    requested_at = getattr(recording, "complete_requested_at", None)
    if requested_at is None:
        return False
    grace = timedelta(seconds=ctx.settings.chunk_grace_seconds)
    return bool(ctx.clock.now() - requested_at > grace)


def fail_chunk(ctx: ServiceContext, recording_id: str, index: int) -> None:
    for _ in range(5):
        chunk = ctx.chunks.get(recording_id, index)
        if chunk is None or chunk.state == ChunkState.FAILED:
            break
        try:
            ctx.chunks.update(chunk.with_changes(state=ChunkState.FAILED))
            break
        except ConcurrencyConflict:
            continue
    _delete_audio(ctx, recording_id, index)


def fail_recording(ctx: ServiceContext, recording_id: str, reason: FailureReason) -> None:
    for _ in range(5):
        recording = ctx.recordings.get(recording_id)
        if recording is None or recording.state in {
            RecordingState.FAILED,
            RecordingState.COMPLETED,
        }:
            return
        try:
            ctx.recordings.update(
                recording.with_changes(
                    state=RecordingState.FAILED,
                    failure_reason=reason,
                    updated_at=ctx.clock.now(),
                )
            )
            logger.warning(
                "recording_failed",
                extra={"recording_id": recording_id, "reason": reason.value},
            )
            return
        except ConcurrencyConflict:
            continue

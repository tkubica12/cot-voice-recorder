"""Shared recording-progress helpers and the exactly-once finalize trigger."""

from __future__ import annotations

from ..domain import Chunk, ChunkState, Recording, RecordingState, can_transition
from ..errors import ConcurrencyConflict
from ..logging_config import get_logger
from .context import ServiceContext
from .messages import finalize_message

logger = get_logger(__name__)


def received_indices(chunks: list[Chunk]) -> set[int]:
    return {c.index for c in chunks}


def transcribed_indices(chunks: list[Chunk]) -> set[int]:
    return {c.index for c in chunks if c.state == ChunkState.TRANSCRIBED}


def all_expected_received(recording: Recording, chunks: list[Chunk]) -> bool:
    expected = recording.expected_chunk_count
    if expected is None:
        return False
    return set(range(expected)).issubset(received_indices(chunks))


def all_expected_transcribed(recording: Recording, chunks: list[Chunk]) -> bool:
    expected = recording.expected_chunk_count
    if expected is None:
        return False
    return set(range(expected)).issubset(transcribed_indices(chunks))


def desired_active_state(recording: Recording, chunks: list[Chunk]) -> RecordingState:
    """State a non-terminal, completion-requested recording should be in."""
    if recording.expected_chunk_count is None:
        return RecordingState.RECORDING
    if all_expected_received(recording, chunks):
        return RecordingState.TRANSCRIBING
    return RecordingState.UPLOADING


def advance_active_state(ctx: ServiceContext, recording_id: str) -> None:
    """Best-effort transition between recording/uploading/transcribing after activity."""
    for _ in range(5):
        recording = ctx.recordings.get(recording_id)
        if recording is None or recording.state in {
            RecordingState.REFINING,
            RecordingState.COMPLETED,
            RecordingState.FAILED,
        }:
            return
        chunks = ctx.chunks.list_for_recording(recording_id)
        target = desired_active_state(recording, chunks)
        if target == recording.state or not can_transition(recording.state, target):
            return
        try:
            ctx.recordings.update(recording.with_changes(state=target, updated_at=ctx.clock.now()))
            return
        except ConcurrencyConflict:
            continue


def try_enqueue_finalize(ctx: ServiceContext, recording_id: str) -> bool:
    """Enqueue the finalize message exactly once when all expected chunks are transcribed.

    Uses optimistic concurrency on the ``finalize_enqueued`` flag so that, under
    concurrent triggers (a transcribe worker and the ``complete`` handler racing), only
    one caller flips the flag and enqueues.

    The flag is persisted *before* the send so concurrent callers cannot both enqueue,
    and rolled back when the send fails so a queue outage cannot permanently strand the
    recording with "already enqueued" state and no message. The send error is re-raised:
    the caller (worker message or ``POST /complete``) retries, and both entry points are
    idempotent.
    """
    for _ in range(5):
        recording = ctx.recordings.get(recording_id)
        if recording is None:
            return False
        if recording.finalize_enqueued or recording.state in {
            RecordingState.REFINING,
            RecordingState.COMPLETED,
            RecordingState.FAILED,
        }:
            return False
        chunks = ctx.chunks.list_for_recording(recording_id)
        if not all_expected_transcribed(recording, chunks):
            return False
        updated = recording.with_changes(
            finalize_enqueued=True,
            state=RecordingState.TRANSCRIBING,
            updated_at=ctx.clock.now(),
        )
        try:
            ctx.recordings.update(updated)
        except ConcurrencyConflict:
            continue
        try:
            ctx.queue.send(finalize_message(recording_id))
        except Exception as exc:
            _release_finalize_flag(ctx, recording_id)
            logger.warning(
                "finalize_enqueue_failed",
                extra={"recording_id": recording_id, "error_type": type(exc).__name__},
            )
            raise
        logger.info("finalize_enqueued", extra={"recording_id": recording_id})
        return True
    return False


def _release_finalize_flag(ctx: ServiceContext, recording_id: str) -> None:
    """Undo the ``finalize_enqueued`` reservation after a failed queue send.

    Re-reads under optimistic concurrency so the rollback never clobbers a concurrent
    writer, and leaves the flag alone once finalization has actually started.
    """
    for _ in range(5):
        current = ctx.recordings.get(recording_id)
        if current is None or not current.finalize_enqueued:
            return
        if current.state in {
            RecordingState.REFINING,
            RecordingState.COMPLETED,
            RecordingState.FAILED,
        }:
            return  # another caller is finalizing; the reservation is genuinely held
        try:
            ctx.recordings.update(
                current.with_changes(finalize_enqueued=False, updated_at=ctx.clock.now())
            )
            return
        except ConcurrencyConflict:
            continue


def arm_finalize(ctx: ServiceContext, recording_id: str) -> None:
    """Guarantee that finalize work exists for a completion-requested recording.

    Enqueues finalize immediately when every expected chunk is already transcribed;
    otherwise (re)arms the delayed watchdog that enforces the missing-chunk grace
    timeout. Safe and cheap to call on every ``POST /complete`` replay: ``finalize`` is
    idempotent, so a duplicate message is harmless, whereas a *missing* message would
    strand the recording forever.
    """
    recording = ctx.recordings.get(recording_id)
    if recording is None or recording.expected_chunk_count is None:
        return
    if recording.state in {RecordingState.COMPLETED, RecordingState.FAILED}:
        return
    if try_enqueue_finalize(ctx, recording_id):
        return
    ctx.queue.send(
        finalize_message(recording_id),
        delay_seconds=ctx.settings.chunk_grace_seconds,
    )

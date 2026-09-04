"""Idempotent retention cleanup — a safety net for inline deletion.

Removes expired transcripts (body + metadata), expired recording metadata (and any
leftover chunk rows / raw text / audio), and stale orphan audio blobs whose recording
is gone or terminal.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import timedelta

from ..domain import RecordingState
from ..logging_config import get_logger
from .context import ServiceContext, raw_path, transcript_path

logger = get_logger(__name__)

_TERMINAL = {RecordingState.COMPLETED, RecordingState.FAILED}


@dataclass(frozen=True, slots=True)
class CleanupResult:
    transcripts_deleted: int = 0
    recordings_deleted: int = 0
    audio_blobs_deleted: int = 0

    def as_dict(self) -> dict[str, int]:
        return {
            "transcripts_deleted": self.transcripts_deleted,
            "recordings_deleted": self.recordings_deleted,
            "audio_blobs_deleted": self.audio_blobs_deleted,
        }


def run_cleanup(ctx: ServiceContext) -> CleanupResult:
    now = ctx.clock.now()
    cutoff = now - timedelta(hours=ctx.settings.retention_hours)
    transcripts_deleted = 0
    recordings_deleted = 0
    audio_deleted = 0

    # 1) Expired transcripts: delete body then metadata.
    for transcript in ctx.transcripts.list_expired(now):
        ctx.blobs.delete(
            ctx.settings.transcript_container, transcript_path(transcript.transcript_id)
        )
        ctx.transcripts.delete(transcript.transcript_id)
        transcripts_deleted += 1

    # 2) Expired recordings: purge chunks, raw text, leftover audio, and metadata.
    for recording in ctx.recordings.list_expired(cutoff):
        rid = recording.recording_id
        for path, _ in ctx.blobs.list_paths(ctx.settings.audio_container, prefix=f"{rid}/"):
            if ctx.blobs.delete(ctx.settings.audio_container, path):
                audio_deleted += 1
        ctx.blobs.delete(ctx.settings.raw_container, raw_path(rid))
        ctx.chunks.delete_for_recording(rid)
        ctx.recordings.delete(rid)
        recordings_deleted += 1

    # 3) Stale orphan audio: recording gone, terminal, or blob older than retention.
    for path, last_modified in ctx.blobs.list_paths(ctx.settings.audio_container):
        recording_id = path.split("/", 1)[0]
        rec = ctx.recordings.get(recording_id)
        stale = rec is None or rec.state in _TERMINAL or last_modified < cutoff
        if stale and ctx.blobs.delete(ctx.settings.audio_container, path):
            audio_deleted += 1

    result = CleanupResult(
        transcripts_deleted=transcripts_deleted,
        recordings_deleted=recordings_deleted,
        audio_blobs_deleted=audio_deleted,
    )
    logger.info("cleanup_completed", extra=result.as_dict())
    return result

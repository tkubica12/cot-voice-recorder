"""Idempotent retention cleanup — a safety net for inline deletion.

Removes expired transcripts (body + metadata), expired recording metadata (and any
leftover chunk rows / raw text / audio), and stale orphan audio blobs whose recording
is gone or terminal.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from datetime import datetime, timedelta

from ..domain import RecordingState
from ..errors import BlobNotFound
from ..logging_config import get_logger
from .context import ServiceContext, raw_path

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
        ctx.blobs.delete(ctx.settings.transcript_container, transcript.body_path)
        ctx.transcripts.delete(transcript.transcript_id)
        transcripts_deleted += 1

    # Failed/conflicting Windows publications can leave immutable envelopes without metadata.
    for path, modified in ctx.blobs.list_paths(
        ctx.settings.transcript_container, prefix="windows-dictation/"
    ):
        parts = path.split("/")
        meta = ctx.transcripts.get(parts[1]) if len(parts) == 3 else None
        if meta is not None and meta.body_path == path:
            continue
        try:
            envelope = json.loads(ctx.blobs.get(ctx.settings.transcript_container, path))
            completed_at = datetime.fromisoformat(envelope["completed_at"])
            if completed_at.tzinfo is None:
                raise ValueError("Missing audit timezone")
            expired = completed_at + timedelta(hours=ctx.settings.retention_hours) <= now
        except BlobNotFound:
            continue
        except (ValueError, KeyError, TypeError):
            logger.error("invalid_orphan_dictation_audit")
            expired = modified < cutoff
        if expired:
            ctx.blobs.delete(ctx.settings.transcript_container, path)

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

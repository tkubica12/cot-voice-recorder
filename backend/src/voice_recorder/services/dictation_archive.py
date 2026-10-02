"""Durable, idempotent Windows audit; deliberately independent of live paste."""

from __future__ import annotations

import hashlib
import json
import uuid
from datetime import timedelta

from ..domain import Transcript
from ..models import DictationArchiveRequest
from ..problems import ConflictError, ValidationProblemError
from ..text import make_preview
from .context import ServiceContext

MAX_BODY_BYTES = 12 * 1024 * 1024 + 16384  # JSON can escape each one-byte control as six bytes.


def archive(ctx: ServiceContext, session_id: str, data: DictationArchiveRequest) -> Transcript:
    transcript_id = str(uuid.UUID(session_id))
    now = ctx.clock.now()
    expires = data.completed_at + timedelta(hours=ctx.settings.retention_hours)
    if data.completed_at > now + timedelta(minutes=5) or expires <= now:
        raise ValidationProblemError("Dictation completion time is invalid or already expired.")
    for text in (data.text, data.raw_text):
        if len(text.encode("utf-8")) > 1024 * 1024:
            raise ValidationProblemError("Dictation text exceeds supported bounds.")
    envelope = json.dumps(
        data.model_dump(mode="json"), ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")
    digest = hashlib.sha256(envelope).hexdigest()
    existing = ctx.transcripts.get(transcript_id)
    if existing is not None:
        if existing.source != "windows_dictation" or existing.content_digest != digest:
            raise ConflictError("A different dictation is already archived for this session.")
        return existing
    # Unique content paths prevent concurrent, conflicting PUTs from overwriting the winner.
    path = f"windows-dictation/{transcript_id}/{digest}.json"
    ctx.blobs.put(
        ctx.settings.transcript_container, path, envelope, content_type="application/json"
    )
    result = ctx.transcripts.create_if_absent(
        Transcript(
            transcript_id=transcript_id,
            recording_id=transcript_id,
            preview=make_preview(data.text, max_chars=ctx.settings.preview_max_chars),
            language=data.language,
            refine_model=ctx.settings.refine_deployment_default if data.polished else "none",
            completed_at=data.completed_at,
            expires_at=expires,
            character_count=len(data.text),
            body_path=path,
            source="windows_dictation",
            transcribe_model=data.transcribe_model,
            content_digest=digest,
        )
    )
    if result.content_digest != digest:
        raise ConflictError("A different dictation won concurrent publication.")
    # No Web PubSub event: cloud audit must never trigger Android-style automatic clipboard copy.
    return result

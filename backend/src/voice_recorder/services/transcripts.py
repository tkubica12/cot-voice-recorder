"""Transcript retrieval and listing service."""

from __future__ import annotations

import base64
import binascii

from ..errors import BlobNotFound
from ..models import Transcript as TranscriptModel
from ..models import TranscriptListPage, TranscriptSummary
from ..problems import BadRequestError, NotFoundError
from .context import ServiceContext


def _encode_cursor(raw: str | None) -> str | None:
    if raw is None:
        return None
    return base64.urlsafe_b64encode(raw.encode("utf-8")).decode("ascii").rstrip("=")


def _decode_cursor(cursor: str | None) -> str | None:
    if cursor is None:
        return None
    padded = cursor + "=" * (-len(cursor) % 4)
    try:
        return base64.urlsafe_b64decode(padded.encode("ascii")).decode("utf-8")
    except (binascii.Error, ValueError, UnicodeDecodeError) as exc:
        raise BadRequestError("Invalid pagination cursor.") from exc


def get_transcript(ctx: ServiceContext, transcript_id: str) -> TranscriptModel:
    meta = ctx.transcripts.get(transcript_id)
    now = ctx.clock.now()
    if meta is None or meta.expires_at <= now:
        raise NotFoundError("Transcript not found or expired.")
    try:
        body = ctx.blobs.get(ctx.settings.transcript_container, meta.body_path).decode("utf-8")
    except BlobNotFound as exc:
        raise NotFoundError("Transcript not found or expired.") from exc
    return TranscriptModel(
        transcript_id=meta.transcript_id,
        recording_id=meta.recording_id,
        body=body,
        preview=meta.preview,
        language=meta.language,
        refine_model=meta.refine_model,
        completed_at=meta.completed_at,
        expires_at=meta.expires_at,
        character_count=meta.character_count,
    )


def list_transcripts(ctx: ServiceContext, *, limit: int, cursor: str | None) -> TranscriptListPage:
    if limit < 1 or limit > 100:
        raise BadRequestError("limit must be between 1 and 100.")
    now = ctx.clock.now()
    items, next_cursor = ctx.transcripts.list_page(limit=limit, cursor=_decode_cursor(cursor))
    summaries = [
        TranscriptSummary(
            transcript_id=item.transcript_id,
            recording_id=item.recording_id,
            preview=item.preview,
            completed_at=item.completed_at,
            expires_at=item.expires_at,
            language=item.language,
            refine_model=item.refine_model,
        )
        for item in items
        if item.expires_at > now
    ]
    return TranscriptListPage(items=summaries, next_cursor=_encode_cursor(next_cursor))

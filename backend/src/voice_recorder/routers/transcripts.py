"""Transcript retrieval routes."""

from __future__ import annotations

from fastapi import APIRouter, Depends, Query

from ..auth import AuthenticatedUser
from ..deps import get_context, require_user
from ..models import Transcript, TranscriptListPage
from ..services.context import ServiceContext
from ..services.transcripts import get_transcript, list_transcripts

router = APIRouter(prefix="/v1/transcripts", tags=["transcripts"])


@router.get("", response_model=TranscriptListPage, operation_id="listTranscripts")
def list_transcripts_route(
    cursor: str | None = Query(default=None),
    limit: int = Query(default=20, ge=1, le=100),
    _user: AuthenticatedUser = Depends(require_user),
    ctx: ServiceContext = Depends(get_context),
) -> TranscriptListPage:
    return list_transcripts(ctx, limit=limit, cursor=cursor)


@router.get("/{transcript_id}", response_model=Transcript, operation_id="getTranscript")
def get_transcript_route(
    transcript_id: str,
    _user: AuthenticatedUser = Depends(require_user),
    ctx: ServiceContext = Depends(get_context),
) -> Transcript:
    return get_transcript(ctx, transcript_id)

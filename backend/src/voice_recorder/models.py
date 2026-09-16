"""Pydantic wire models that mirror ``openapi/voice-recorder.yaml`` exactly."""

from __future__ import annotations

from datetime import datetime
from typing import Literal

from pydantic import BaseModel, ConfigDict, Field

from .dictation_edits import MAX_EDIT_CHARS, MAX_EDITS, MAX_PREVIOUS_CHARS, MAX_TEXT_CHARS

RefineModel = Literal["gpt-5.6-luna", "gpt-5.6-terra"]


class StrictModel(BaseModel):
    model_config = ConfigDict(extra="forbid")


class HealthStatus(StrictModel):
    status: Literal["ok", "not_ready"]


class DictationResult(StrictModel):
    text: str


class DictationEdit(StrictModel):
    original: str = Field(..., min_length=1, max_length=MAX_EDIT_CHARS)
    replacement: str = Field(..., max_length=MAX_EDIT_CHARS)


class DictationRefineResult(StrictModel):
    text: str
    edits: list[DictationEdit] = Field(..., max_length=MAX_EDITS)


class DictationRefineRequest(StrictModel):
    text: str = Field(..., strict=True, min_length=1, max_length=MAX_TEXT_CHARS, pattern=r"\S")
    previous_text: str = Field("", strict=True, max_length=MAX_PREVIOUS_CHARS)


class ClientInfo(StrictModel):
    platform: Literal["android", "windows"]
    app_version: str | None = None


class CreateRecordingRequest(StrictModel):
    client_recording_id: str = Field(..., description="Client idempotency key (UUID).")
    refine_model: RefineModel = "gpt-5.6-luna"
    language: str = "cs"
    client: ClientInfo | None = None
    started_at: datetime | None = None


class RecordingProgress(StrictModel):
    expected_chunk_count: int | None = Field(None, ge=0)
    received_chunk_count: int = Field(..., ge=0)
    transcribed_chunk_count: int = Field(..., ge=0)


class Recording(StrictModel):
    recording_id: str
    client_recording_id: str
    state: Literal["recording", "uploading", "transcribing", "refining", "completed", "failed"]
    refine_model: RefineModel
    language: str
    progress: RecordingProgress
    transcript_id: str | None = None
    failure_reason: (
        Literal[
            "missing_chunks",
            "transcription_failed",
            "refinement_failed",
            "internal_error",
        ]
        | None
    ) = None
    created_at: datetime
    updated_at: datetime


class ChunkAccepted(StrictModel):
    recording_id: str
    index: int = Field(..., ge=0)
    chunk_state: Literal["accepted", "transcribed", "failed"]
    checksum: str
    received_at: datetime


class CompleteRecordingRequest(StrictModel):
    chunk_count: int = Field(..., ge=1)
    stopped_at: datetime | None = None


class TranscriptSummary(StrictModel):
    transcript_id: str
    recording_id: str
    preview: str = Field(..., max_length=140)
    completed_at: datetime
    expires_at: datetime
    language: str
    refine_model: RefineModel


class Transcript(StrictModel):
    transcript_id: str
    recording_id: str
    body: str
    preview: str = Field(..., max_length=140)
    language: str
    refine_model: RefineModel
    completed_at: datetime
    expires_at: datetime
    character_count: int = Field(..., ge=0)


class TranscriptListPage(StrictModel):
    items: list[TranscriptSummary]
    next_cursor: str | None = None


class NegotiateRequest(StrictModel):
    client: ClientInfo | None = None


class NegotiateResponse(StrictModel):
    url: str
    hub: str
    group: str
    expires_at: datetime


class TranscriptCompletedEvent(StrictModel):
    event: Literal["transcript.completed"] = "transcript.completed"
    event_id: str
    transcript_id: str
    recording_id: str
    completed_at: datetime
    preview: str = Field(..., max_length=140)

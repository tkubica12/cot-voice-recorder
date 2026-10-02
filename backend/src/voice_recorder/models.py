"""Pydantic wire models that mirror ``openapi/voice-recorder.yaml`` exactly."""

from __future__ import annotations

from datetime import datetime
from typing import Literal

from pydantic import AwareDatetime, BaseModel, ConfigDict, Field

from .dictation_edits import MAX_EDIT_CHARS, MAX_EDITS, MAX_PREVIOUS_CHARS, MAX_TEXT_CHARS

RefineModel = Literal["gpt-6-luna", "gpt-5.6-luna", "gpt-5.6-terra"]
TranscriptRefineModel = Literal["gpt-6-luna", "gpt-5.6-luna", "gpt-5.6-terra", "none"]


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


class DictationArchiveRequest(StrictModel):
    text: str = Field(..., strict=True, min_length=1, max_length=1_048_576, pattern=r"\S")
    raw_text: str = Field(..., strict=True, min_length=1, max_length=1_048_576, pattern=r"\S")
    language: Literal["auto", "cs", "en"] = "auto"
    completed_at: AwareDatetime
    polished: bool = False
    transcribe_model: Literal[
        "MAI-Transcribe-2-Streaming",
        "MAI-Transcribe-2",
        "MAI-Transcribe-2-Streaming+MAI-Transcribe-2",
    ] = "MAI-Transcribe-2-Streaming"


class ClientInfo(StrictModel):
    platform: Literal["android", "windows"]
    app_version: str | None = None


class CreateRecordingRequest(StrictModel):
    client_recording_id: str = Field(..., description="Client idempotency key (UUID).")
    refine_model: RefineModel = "gpt-6-luna"
    language: str = "cs"
    client: ClientInfo | None = None
    started_at: datetime | None = None
    transcription_mode: Literal["chunked", "streaming"] = "chunked"
    audio_layout: Literal["legacy_overlap", "contiguous"] = "legacy_overlap"
    refinement_enabled: bool = True


class RecordingProgress(StrictModel):
    expected_chunk_count: int | None = Field(None, ge=0)
    received_chunk_count: int = Field(..., ge=0)
    transcribed_chunk_count: int = Field(..., ge=0)
    streamed_audio_ms: int = Field(0, ge=0)
    stream_preview: str = Field("", max_length=140)
    stream_attempt: int = Field(0, ge=0)
    stream_error: str | None = None


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
    transcription_mode: Literal["chunked", "streaming"] = "chunked"
    audio_layout: Literal["legacy_overlap", "contiguous"] = "legacy_overlap"
    refinement_enabled: bool = True


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
    refine_model: TranscriptRefineModel
    source: Literal["recording", "windows_dictation"] = "recording"
    transcribe_model: str | None = None


class Transcript(StrictModel):
    transcript_id: str
    recording_id: str
    body: str
    preview: str = Field(..., max_length=140)
    language: str
    refine_model: TranscriptRefineModel
    completed_at: datetime
    expires_at: datetime
    character_count: int = Field(..., ge=0)
    source: Literal["recording", "windows_dictation"] = "recording"
    transcribe_model: str | None = None
    raw_body: str | None = None


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

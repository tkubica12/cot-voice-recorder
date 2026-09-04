"""Domain entities, enums, and the recording state machine.

These are storage-agnostic value objects used by services and repositories. They are
deliberately separate from the wire (pydantic) models so that persistence details never
leak into the API surface.
"""

from __future__ import annotations

import enum
from dataclasses import dataclass, field, replace
from datetime import datetime


class RecordingState(enum.StrEnum):
    RECORDING = "recording"
    UPLOADING = "uploading"
    TRANSCRIBING = "transcribing"
    REFINING = "refining"
    COMPLETED = "completed"
    FAILED = "failed"


TERMINAL_STATES = frozenset({RecordingState.COMPLETED, RecordingState.FAILED})


class ChunkState(enum.StrEnum):
    ACCEPTED = "accepted"
    TRANSCRIBED = "transcribed"
    FAILED = "failed"


class FailureReason(enum.StrEnum):
    MISSING_CHUNKS = "missing_chunks"
    TRANSCRIPTION_FAILED = "transcription_failed"
    REFINEMENT_FAILED = "refinement_failed"
    INTERNAL_ERROR = "internal_error"


# Allowed forward transitions. ``failed`` is reachable from any non-terminal state.
_ALLOWED: dict[RecordingState, frozenset[RecordingState]] = {
    RecordingState.RECORDING: frozenset(
        {RecordingState.UPLOADING, RecordingState.TRANSCRIBING, RecordingState.FAILED}
    ),
    RecordingState.UPLOADING: frozenset({RecordingState.TRANSCRIBING, RecordingState.FAILED}),
    RecordingState.TRANSCRIBING: frozenset({RecordingState.REFINING, RecordingState.FAILED}),
    RecordingState.REFINING: frozenset({RecordingState.COMPLETED, RecordingState.FAILED}),
    RecordingState.COMPLETED: frozenset(),
    RecordingState.FAILED: frozenset(),
}


def can_transition(src: RecordingState, dst: RecordingState) -> bool:
    if src == dst:
        return True
    return dst in _ALLOWED[src]


@dataclass(frozen=True, slots=True)
class Recording:
    recording_id: str
    client_recording_id: str
    state: RecordingState
    refine_model: str
    language: str
    created_at: datetime
    updated_at: datetime
    expected_chunk_count: int | None = None
    transcript_id: str | None = None
    failure_reason: FailureReason | None = None
    complete_requested_at: datetime | None = None
    finalize_enqueued: bool = False
    # Opaque optimistic-concurrency token (Table ETag / in-memory version).
    etag: str | None = None

    def with_changes(self, **changes: object) -> Recording:
        return replace(self, **changes)  # type: ignore[arg-type]


@dataclass(frozen=True, slots=True)
class Chunk:
    recording_id: str
    index: int
    checksum: str
    state: ChunkState
    received_at: datetime
    size_bytes: int
    blob_path: str | None = None
    text: str | None = None
    duration_ms: int | None = None
    overlap_ms: int | None = None
    started_at: datetime | None = None
    etag: str | None = None

    def with_changes(self, **changes: object) -> Chunk:
        return replace(self, **changes)  # type: ignore[arg-type]


@dataclass(frozen=True, slots=True)
class Transcript:
    transcript_id: str
    recording_id: str
    preview: str
    language: str
    refine_model: str
    completed_at: datetime
    expires_at: datetime
    character_count: int
    body_path: str
    etag: str | None = None


@dataclass(frozen=True, slots=True)
class TranscriptPage:
    items: list[Transcript] = field(default_factory=list)
    next_cursor: str | None = None

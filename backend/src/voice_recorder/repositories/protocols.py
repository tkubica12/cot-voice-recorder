"""Repository protocols. Implementations: in-memory (tests) and Azure Table (prod)."""

from __future__ import annotations

from datetime import datetime
from typing import Protocol

from ..domain import Chunk, Recording, Transcript


class RecordingRepository(Protocol):
    def create_if_absent(self, recording: Recording) -> tuple[Recording, bool]:
        """Insert keyed by ``client_recording_id``; return (entity, created)."""
        ...

    def get(self, recording_id: str) -> Recording | None: ...

    def get_by_client_id(self, client_recording_id: str) -> Recording | None: ...

    def update(self, recording: Recording) -> Recording:
        """Persist using optimistic concurrency; raise ConcurrencyConflict on mismatch."""
        ...

    def list_expired(self, cutoff: datetime) -> list[Recording]:
        """Recordings whose ``updated_at`` is older than ``cutoff``."""
        ...

    def delete(self, recording_id: str) -> None: ...


class ChunkRepository(Protocol):
    def put_if_absent(self, chunk: Chunk) -> tuple[Chunk, bool]:
        """Insert keyed by (recording_id, index); return (entity, created)."""
        ...

    def get(self, recording_id: str, index: int) -> Chunk | None: ...

    def update(self, chunk: Chunk) -> Chunk: ...

    def list_for_recording(self, recording_id: str) -> list[Chunk]: ...

    def delete_for_recording(self, recording_id: str) -> None: ...


class TranscriptRepository(Protocol):
    def create(self, transcript: Transcript) -> Transcript: ...

    def get(self, transcript_id: str) -> Transcript | None: ...

    def list_page(self, *, limit: int, cursor: str | None) -> tuple[list[Transcript], str | None]:
        """Newest-first page; returns (items, next_cursor)."""
        ...

    def list_expired(self, now: datetime) -> list[Transcript]: ...

    def delete(self, transcript_id: str) -> None: ...

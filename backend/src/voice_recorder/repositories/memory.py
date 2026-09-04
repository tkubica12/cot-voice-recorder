"""Thread-safe in-memory repositories with optimistic concurrency.

Used by unit/API tests and by the gated local mode. ETags are simple monotonically
increasing version strings per entity.
"""

from __future__ import annotations

import threading
from datetime import datetime

from ..domain import Chunk, Recording, Transcript
from ..errors import ConcurrencyConflict


def _next_etag(current: str | None) -> str:
    return "1" if current is None else str(int(current) + 1)


class InMemoryRecordingRepository:
    def __init__(self) -> None:
        self._lock = threading.RLock()
        self._by_id: dict[str, Recording] = {}
        self._crid_index: dict[str, str] = {}

    def create_if_absent(self, recording: Recording) -> tuple[Recording, bool]:
        with self._lock:
            existing_id = self._crid_index.get(recording.client_recording_id)
            if existing_id is not None:
                return self._by_id[existing_id], False
            stored = recording.with_changes(etag="1")
            self._by_id[stored.recording_id] = stored
            self._crid_index[stored.client_recording_id] = stored.recording_id
            return stored, True

    def get(self, recording_id: str) -> Recording | None:
        with self._lock:
            return self._by_id.get(recording_id)

    def get_by_client_id(self, client_recording_id: str) -> Recording | None:
        with self._lock:
            rid = self._crid_index.get(client_recording_id)
            return self._by_id.get(rid) if rid else None

    def update(self, recording: Recording) -> Recording:
        with self._lock:
            current = self._by_id.get(recording.recording_id)
            if current is None:
                raise ConcurrencyConflict("recording no longer exists")
            if recording.etag != current.etag:
                raise ConcurrencyConflict("recording etag mismatch")
            stored = recording.with_changes(etag=_next_etag(current.etag))
            self._by_id[stored.recording_id] = stored
            return stored

    def list_expired(self, cutoff: datetime) -> list[Recording]:
        with self._lock:
            return [r for r in self._by_id.values() if r.updated_at < cutoff]

    def delete(self, recording_id: str) -> None:
        with self._lock:
            rec = self._by_id.pop(recording_id, None)
            if rec is not None:
                self._crid_index.pop(rec.client_recording_id, None)


class InMemoryChunkRepository:
    def __init__(self) -> None:
        self._lock = threading.RLock()
        self._chunks: dict[tuple[str, int], Chunk] = {}

    def put_if_absent(self, chunk: Chunk) -> tuple[Chunk, bool]:
        key = (chunk.recording_id, chunk.index)
        with self._lock:
            existing = self._chunks.get(key)
            if existing is not None:
                return existing, False
            stored = chunk.with_changes(etag="1")
            self._chunks[key] = stored
            return stored, True

    def get(self, recording_id: str, index: int) -> Chunk | None:
        with self._lock:
            return self._chunks.get((recording_id, index))

    def update(self, chunk: Chunk) -> Chunk:
        key = (chunk.recording_id, chunk.index)
        with self._lock:
            current = self._chunks.get(key)
            if current is None:
                raise ConcurrencyConflict("chunk no longer exists")
            if chunk.etag != current.etag:
                raise ConcurrencyConflict("chunk etag mismatch")
            stored = chunk.with_changes(etag=_next_etag(current.etag))
            self._chunks[key] = stored
            return stored

    def list_for_recording(self, recording_id: str) -> list[Chunk]:
        with self._lock:
            return sorted(
                (c for c in self._chunks.values() if c.recording_id == recording_id),
                key=lambda c: c.index,
            )

    def delete_for_recording(self, recording_id: str) -> None:
        with self._lock:
            for key in [k for k in self._chunks if k[0] == recording_id]:
                del self._chunks[key]


class InMemoryTranscriptRepository:
    def __init__(self) -> None:
        self._lock = threading.RLock()
        self._by_id: dict[str, Transcript] = {}

    def create(self, transcript: Transcript) -> Transcript:
        with self._lock:
            stored = transcript
            self._by_id[transcript.transcript_id] = stored
            return stored

    def get(self, transcript_id: str) -> Transcript | None:
        with self._lock:
            return self._by_id.get(transcript_id)

    def list_page(self, *, limit: int, cursor: str | None) -> tuple[list[Transcript], str | None]:
        with self._lock:
            ordered = sorted(
                self._by_id.values(),
                key=lambda t: (t.completed_at, t.transcript_id),
                reverse=True,
            )
        start = 0
        if cursor is not None:
            for i, item in enumerate(ordered):
                token = f"{item.completed_at.isoformat()}|{item.transcript_id}"
                if token == cursor:
                    start = i + 1
                    break
        page = ordered[start : start + limit]
        next_cursor: str | None = None
        if start + limit < len(ordered) and page:
            last = page[-1]
            next_cursor = f"{last.completed_at.isoformat()}|{last.transcript_id}"
        return page, next_cursor

    def list_expired(self, now: datetime) -> list[Transcript]:
        with self._lock:
            return [t for t in self._by_id.values() if t.expires_at <= now]

    def delete(self, transcript_id: str) -> None:
        with self._lock:
            self._by_id.pop(transcript_id, None)

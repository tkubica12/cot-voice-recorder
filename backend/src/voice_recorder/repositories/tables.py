"""Azure Table Storage repositories with optimistic concurrency (ETag)."""

from __future__ import annotations

import contextlib
from datetime import datetime
from typing import Any

from azure.core import MatchConditions
from azure.core.exceptions import (
    ResourceExistsError,
    ResourceModifiedError,
    ResourceNotFoundError,
)
from azure.data.tables import TableClient, UpdateMode

from ..domain import (
    Chunk,
    ChunkState,
    FailureReason,
    Recording,
    RecordingState,
    Transcript,
)
from ..errors import ConcurrencyConflict

_RECORDING_PK = "recording"
_CRID_PK = "crid"
_TRANSCRIPT_PK = "transcript"


def _clean(entity: dict[str, Any]) -> dict[str, Any]:
    return {k: v for k, v in entity.items() if v is not None}


class AzureTableRecordingRepository:
    def __init__(self, client: TableClient) -> None:
        self._client = client
        with contextlib.suppress(ResourceExistsError):
            self._client.create_table()

    def create_if_absent(self, recording: Recording) -> tuple[Recording, bool]:
        index = {
            "PartitionKey": _CRID_PK,
            "RowKey": recording.client_recording_id,
            "recording_id": recording.recording_id,
        }
        try:
            self._client.create_entity(index)
        except ResourceExistsError:
            existing = self.get_by_client_id(recording.client_recording_id)
            if existing is not None:
                return existing, False
            # Index exists but the recording row is missing; fall through to create it.
        try:
            self._client.create_entity(_to_recording_entity(recording))
        except ResourceExistsError:
            existing = self.get(recording.recording_id)
            if existing is not None:
                return existing, False
        stored = self.get(recording.recording_id)
        assert stored is not None
        return stored, True

    def get(self, recording_id: str) -> Recording | None:
        try:
            entity = self._client.get_entity(_RECORDING_PK, recording_id)
        except ResourceNotFoundError:
            return None
        return _from_recording_entity(entity)

    def get_by_client_id(self, client_recording_id: str) -> Recording | None:
        try:
            index = self._client.get_entity(_CRID_PK, client_recording_id)
        except ResourceNotFoundError:
            return None
        return self.get(str(index["recording_id"]))

    def update(self, recording: Recording) -> Recording:
        entity = _to_recording_entity(recording)
        try:
            self._client.update_entity(
                entity,
                mode=UpdateMode.REPLACE,
                etag=recording.etag,
                match_condition=MatchConditions.IfNotModified,
            )
        except ResourceModifiedError as exc:
            raise ConcurrencyConflict("recording etag mismatch") from exc
        except ResourceNotFoundError as exc:
            raise ConcurrencyConflict("recording no longer exists") from exc
        stored = self.get(recording.recording_id)
        assert stored is not None
        return stored

    def list_expired(self, cutoff: datetime) -> list[Recording]:
        entities = self._client.query_entities(f"PartitionKey eq '{_RECORDING_PK}'")
        result: list[Recording] = []
        for entity in entities:
            recording = _from_recording_entity(entity)
            if recording.updated_at < cutoff:
                result.append(recording)
        return result

    def delete(self, recording_id: str) -> None:
        recording = self.get(recording_id)
        with contextlib.suppress(ResourceNotFoundError):
            self._client.delete_entity(_RECORDING_PK, recording_id)
        if recording is not None:
            with contextlib.suppress(ResourceNotFoundError):
                self._client.delete_entity(_CRID_PK, recording.client_recording_id)


class AzureTableChunkRepository:
    def __init__(self, client: TableClient) -> None:
        self._client = client
        with contextlib.suppress(ResourceExistsError):
            self._client.create_table()

    def put_if_absent(self, chunk: Chunk) -> tuple[Chunk, bool]:
        try:
            self._client.create_entity(_to_chunk_entity(chunk))
        except ResourceExistsError:
            existing = self.get(chunk.recording_id, chunk.index)
            if existing is not None:
                return existing, False
        stored = self.get(chunk.recording_id, chunk.index)
        assert stored is not None
        return stored, True

    def get(self, recording_id: str, index: int) -> Chunk | None:
        try:
            entity = self._client.get_entity(recording_id, _chunk_rk(index))
        except ResourceNotFoundError:
            return None
        return _from_chunk_entity(entity)

    def update(self, chunk: Chunk) -> Chunk:
        try:
            self._client.update_entity(
                _to_chunk_entity(chunk),
                mode=UpdateMode.REPLACE,
                etag=chunk.etag,
                match_condition=MatchConditions.IfNotModified,
            )
        except ResourceModifiedError as exc:
            raise ConcurrencyConflict("chunk etag mismatch") from exc
        except ResourceNotFoundError as exc:
            raise ConcurrencyConflict("chunk no longer exists") from exc
        stored = self.get(chunk.recording_id, chunk.index)
        assert stored is not None
        return stored

    def list_for_recording(self, recording_id: str) -> list[Chunk]:
        entities = self._client.query_entities(f"PartitionKey eq '{recording_id}'")
        chunks = [_from_chunk_entity(e) for e in entities]
        return sorted(chunks, key=lambda c: c.index)

    def delete_for_recording(self, recording_id: str) -> None:
        for chunk in self.list_for_recording(recording_id):
            with contextlib.suppress(ResourceNotFoundError):
                self._client.delete_entity(recording_id, _chunk_rk(chunk.index))


class AzureTableTranscriptRepository:
    def __init__(self, client: TableClient) -> None:
        self._client = client
        with contextlib.suppress(ResourceExistsError):
            self._client.create_table()

    def create(self, transcript: Transcript) -> Transcript:
        self._client.upsert_entity(_to_transcript_entity(transcript))
        return transcript

    def get(self, transcript_id: str) -> Transcript | None:
        try:
            entity = self._client.get_entity(_TRANSCRIPT_PK, transcript_id)
        except ResourceNotFoundError:
            return None
        return _from_transcript_entity(entity)

    def list_page(self, *, limit: int, cursor: str | None) -> tuple[list[Transcript], str | None]:
        entities = self._client.query_entities(f"PartitionKey eq '{_TRANSCRIPT_PK}'")
        ordered = sorted(
            (_from_transcript_entity(e) for e in entities),
            key=lambda t: (t.completed_at, t.transcript_id),
            reverse=True,
        )
        start = 0
        if cursor is not None:
            for i, item in enumerate(ordered):
                if f"{item.completed_at.isoformat()}|{item.transcript_id}" == cursor:
                    start = i + 1
                    break
        page = ordered[start : start + limit]
        next_cursor: str | None = None
        if start + limit < len(ordered) and page:
            last = page[-1]
            next_cursor = f"{last.completed_at.isoformat()}|{last.transcript_id}"
        return page, next_cursor

    def list_expired(self, now: datetime) -> list[Transcript]:
        entities = self._client.query_entities(f"PartitionKey eq '{_TRANSCRIPT_PK}'")
        return [t for t in (_from_transcript_entity(e) for e in entities) if t.expires_at <= now]

    def delete(self, transcript_id: str) -> None:
        with contextlib.suppress(ResourceNotFoundError):
            self._client.delete_entity(_TRANSCRIPT_PK, transcript_id)


def _chunk_rk(index: int) -> str:
    return f"{index:08d}"


def _to_recording_entity(recording: Recording) -> dict[str, Any]:
    return _clean(
        {
            "PartitionKey": _RECORDING_PK,
            "RowKey": recording.recording_id,
            "recording_id": recording.recording_id,
            "client_recording_id": recording.client_recording_id,
            "state": recording.state.value,
            "refine_model": recording.refine_model,
            "language": recording.language,
            "created_at": recording.created_at,
            "updated_at": recording.updated_at,
            "expected_chunk_count": recording.expected_chunk_count,
            "transcript_id": recording.transcript_id,
            "failure_reason": (
                recording.failure_reason.value if recording.failure_reason else None
            ),
            "complete_requested_at": recording.complete_requested_at,
            "finalize_enqueued": recording.finalize_enqueued,
        }
    )


def _from_recording_entity(entity: Any) -> Recording:
    failure = entity.get("failure_reason")
    return Recording(
        recording_id=str(entity["recording_id"]),
        client_recording_id=str(entity["client_recording_id"]),
        state=RecordingState(str(entity["state"])),
        refine_model=str(entity["refine_model"]),
        language=str(entity["language"]),
        created_at=entity["created_at"],
        updated_at=entity["updated_at"],
        expected_chunk_count=entity.get("expected_chunk_count"),
        transcript_id=entity.get("transcript_id"),
        failure_reason=FailureReason(failure) if failure else None,
        complete_requested_at=entity.get("complete_requested_at"),
        finalize_enqueued=bool(entity.get("finalize_enqueued", False)),
        etag=entity.metadata.get("etag") if hasattr(entity, "metadata") else None,
    )


def _to_chunk_entity(chunk: Chunk) -> dict[str, Any]:
    return _clean(
        {
            "PartitionKey": chunk.recording_id,
            "RowKey": _chunk_rk(chunk.index),
            "index": chunk.index,
            "checksum": chunk.checksum,
            "state": chunk.state.value,
            "received_at": chunk.received_at,
            "size_bytes": chunk.size_bytes,
            "blob_path": chunk.blob_path,
            "text": chunk.text,
            "duration_ms": chunk.duration_ms,
            "overlap_ms": chunk.overlap_ms,
            "started_at": chunk.started_at,
        }
    )


def _from_chunk_entity(entity: Any) -> Chunk:
    return Chunk(
        recording_id=str(entity["PartitionKey"]),
        index=int(entity["index"]),
        checksum=str(entity["checksum"]),
        state=ChunkState(str(entity["state"])),
        received_at=entity["received_at"],
        size_bytes=int(entity["size_bytes"]),
        blob_path=entity.get("blob_path"),
        text=entity.get("text"),
        duration_ms=entity.get("duration_ms"),
        overlap_ms=entity.get("overlap_ms"),
        started_at=entity.get("started_at"),
        etag=entity.metadata.get("etag") if hasattr(entity, "metadata") else None,
    )


def _to_transcript_entity(transcript: Transcript) -> dict[str, Any]:
    return _clean(
        {
            "PartitionKey": _TRANSCRIPT_PK,
            "RowKey": transcript.transcript_id,
            "recording_id": transcript.recording_id,
            "preview": transcript.preview,
            "language": transcript.language,
            "refine_model": transcript.refine_model,
            "completed_at": transcript.completed_at,
            "expires_at": transcript.expires_at,
            "character_count": transcript.character_count,
            "body_path": transcript.body_path,
        }
    )


def _from_transcript_entity(entity: Any) -> Transcript:
    return Transcript(
        transcript_id=str(entity["RowKey"]),
        recording_id=str(entity["recording_id"]),
        preview=str(entity["preview"]),
        language=str(entity["language"]),
        refine_model=str(entity["refine_model"]),
        completed_at=entity["completed_at"],
        expires_at=entity["expires_at"],
        character_count=int(entity["character_count"]),
        body_path=str(entity["body_path"]),
        etag=entity.metadata.get("etag") if hasattr(entity, "metadata") else None,
    )

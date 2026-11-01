from datetime import UTC, datetime

from azure.data.tables._deserialize import _convert_to_entity

from voice_recorder.repositories.tables import (
    _from_chunk_entity,
    _from_recording_entity,
    _from_transcript_entity,
)


def test_recording_reader_accepts_actual_sdk_int64_values() -> None:
    entity = _convert_to_entity(
        {
            "PartitionKey": "recording",
            "RowKey": "fixture",
            "recording_id": "fixture",
            "client_recording_id": "client",
            "state": "recording",
            "refine_model": "gpt-6-luna",
            "language": "cs",
            "created_at": "2026-11-01T10:00:00Z",
            "created_at@odata.type": "Edm.DateTime",
            "updated_at": "2026-11-01T10:00:00Z",
            "updated_at@odata.type": "Edm.DateTime",
            "streamed_samples": "2700000000",
            "streamed_samples@odata.type": "Edm.Int64",
            "stream_attempt": "3",
            "stream_attempt@odata.type": "Edm.Int64",
            "expected_chunk_count": "200",
            "expected_chunk_count@odata.type": "Edm.Int64",
        }
    )
    record = _from_recording_entity(entity)
    assert record.streamed_samples == 2_700_000_000
    assert record.stream_attempt == 3
    assert record.expected_chunk_count == 200
    assert record.created_at == datetime(2026, 11, 1, 10, 0, tzinfo=UTC)


def test_chunk_reader_unwraps_integer_metadata_without_changing_absence() -> None:
    entity = _convert_to_entity(
        {
            "PartitionKey": "fixture",
            "RowKey": "00000002",
            "index": "2",
            "index@odata.type": "Edm.Int64",
            "checksum": "synthetic-digest",
            "state": "accepted",
            "received_at": "2026-11-01T10:00:00Z",
            "received_at@odata.type": "Edm.DateTime",
            "size_bytes": "320044",
            "size_bytes@odata.type": "Edm.Int64",
            "start_sample": "320000",
            "start_sample@odata.type": "Edm.Int64",
            "sample_count": "160000",
            "sample_count@odata.type": "Edm.Int64",
        }
    )
    chunk = _from_chunk_entity(entity)
    assert chunk.index == 2
    assert chunk.size_bytes == 320044
    assert chunk.start_sample == 320000
    assert chunk.sample_count == 160000
    assert chunk.duration_ms is None
    assert chunk.overlap_ms is None


def test_transcript_reader_accepts_sdk_int64_character_count() -> None:
    entity = _convert_to_entity(
        {
            "PartitionKey": "transcript",
            "RowKey": "fixture",
            "recording_id": "fixture",
            "preview": "synthetic",
            "language": "auto",
            "refine_model": "none",
            "completed_at": "2026-11-01T10:00:00Z",
            "completed_at@odata.type": "Edm.DateTime",
            "expires_at": "2026-11-03T10:00:00Z",
            "expires_at@odata.type": "Edm.DateTime",
            "character_count": "9",
            "character_count@odata.type": "Edm.Int64",
            "body_path": "fixture.txt",
        }
    )
    assert _from_transcript_entity(entity).character_count == 9

import pytest

from voice_recorder.digest import compute_content_digest
from voice_recorder.domain import RecordingState
from voice_recorder.problems import CompleteConflictError
from voice_recorder.services.context import ServiceContext
from voice_recorder.services.recordings import (
    complete_recording,
    create_recording,
    get_recording,
    upload_chunk,
)

from .conftest import drain, unique_wav


def _new_recording(ctx: ServiceContext) -> str:
    rec, _ = create_recording(
        ctx,
        client_recording_id="crid-" + str(id(ctx)),
        refine_model="gpt-5.6-luna",
        language="cs",
    )
    return rec.recording_id


def _upload(ctx: ServiceContext, rid: str, index: int, text: str) -> None:
    data = unique_wav(index)
    ctx.transcriber.responses[data] = text  # type: ignore[attr-defined]
    upload_chunk(
        ctx,
        recording_id=rid,
        index=index,
        data=data,
        checksum=compute_content_digest(data),
        duration_ms=30000,
        overlap_ms=1500,
        started_at=None,
    )


def test_complete_before_chunks_then_finalizes(context: ServiceContext) -> None:
    rid = _new_recording(context)
    rec, status = complete_recording(context, recording_id=rid, chunk_count=2)
    assert status == 202
    assert rec.state == RecordingState.UPLOADING
    assert rec.expected_chunk_count == 2

    _upload(context, rid, 0, "prvni cast textu")
    _upload(context, rid, 1, "textu a pokracovani")
    drain(context)

    final = get_recording(context, rid)
    assert final.state == RecordingState.COMPLETED
    assert final.transcript_id is not None
    assert len(context.realtime.sent) == 1  # type: ignore[attr-defined]


def test_chunks_before_complete_then_finalizes(context: ServiceContext) -> None:
    rid = _new_recording(context)
    _upload(context, rid, 0, "prvni cast textu")
    _upload(context, rid, 1, "textu a pokracovani")
    drain(context)  # transcribe both; not finalized yet (no complete)

    mid = get_recording(context, rid)
    assert mid.state == RecordingState.RECORDING
    assert mid.transcript_id is None

    _rec, status = complete_recording(context, recording_id=rid, chunk_count=2)
    assert status == 202
    drain(context)

    final = get_recording(context, rid)
    assert final.state == RecordingState.COMPLETED
    assert len(context.realtime.sent) == 1  # type: ignore[attr-defined]


def test_finalize_runs_exactly_once(context: ServiceContext) -> None:
    rid = _new_recording(context)
    _upload(context, rid, 0, "alpha beta")
    _upload(context, rid, 1, "beta gamma")
    complete_recording(context, recording_id=rid, chunk_count=2)
    drain(context)
    drain(context)  # extra drains must not re-finalize

    assert len(context.realtime.sent) == 1  # type: ignore[attr-defined]
    transcripts, _ = context.transcripts.list_page(limit=10, cursor=None)
    assert len(transcripts) == 1


def test_complete_idempotent_same_count(context: ServiceContext) -> None:
    rid = _new_recording(context)
    _rec, first = complete_recording(context, recording_id=rid, chunk_count=3)
    _rec2, second = complete_recording(context, recording_id=rid, chunk_count=3)
    assert first == 202
    assert second == 200


def test_complete_conflicting_count(context: ServiceContext) -> None:
    rid = _new_recording(context)
    complete_recording(context, recording_id=rid, chunk_count=3)
    with pytest.raises(CompleteConflictError):
        complete_recording(context, recording_id=rid, chunk_count=4)


def test_complete_smaller_than_received_conflicts(context: ServiceContext) -> None:
    rid = _new_recording(context)
    _upload(context, rid, 0, "a")
    _upload(context, rid, 1, "b")
    with pytest.raises(CompleteConflictError):
        complete_recording(context, recording_id=rid, chunk_count=1)

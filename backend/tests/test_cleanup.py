from voice_recorder.digest import compute_content_digest
from voice_recorder.services.cleanup import run_cleanup
from voice_recorder.services.context import ServiceContext, raw_path, transcript_path
from voice_recorder.services.recordings import (
    complete_recording,
    create_recording,
    upload_chunk,
)

from .conftest import FakeClock, drain, unique_wav


def _produce(ctx: ServiceContext, seed: int) -> str:
    rec, _ = create_recording(
        ctx, client_recording_id=f"crid-c{seed}", refine_model="gpt-5.6-luna", language="cs"
    )
    rid = rec.recording_id
    data = unique_wav(seed)
    ctx.transcriber.responses[data] = "some transcript text"  # type: ignore[attr-defined]
    upload_chunk(
        ctx,
        recording_id=rid,
        index=0,
        data=data,
        checksum=compute_content_digest(data),
        duration_ms=30000,
        overlap_ms=1500,
        started_at=None,
    )
    complete_recording(ctx, recording_id=rid, chunk_count=1)
    drain(ctx)
    return rid


def test_cleanup_removes_expired_transcript_and_recording(
    context: ServiceContext, clock: FakeClock
) -> None:
    rid = _produce(context, 1)
    tid = (context.recordings.get(rid) or None).transcript_id  # type: ignore[union-attr]
    assert tid is not None

    clock.advance(hours=context.settings.retention_hours + 1)
    result = run_cleanup(context)

    assert result.transcripts_deleted >= 1
    assert result.recordings_deleted >= 1
    assert context.transcripts.get(tid) is None
    assert context.recordings.get(rid) is None
    assert not context.blobs.exists(context.settings.transcript_container, transcript_path(tid))
    assert not context.blobs.exists(context.settings.raw_container, raw_path(rid))


def test_cleanup_removes_orphan_audio(context: ServiceContext) -> None:
    context.blobs.put(
        context.settings.audio_container, "ghost/0.wav", b"RIFFxxxx", content_type="audio/wav"
    )
    result = run_cleanup(context)
    assert result.audio_blobs_deleted >= 1
    assert not context.blobs.exists(context.settings.audio_container, "ghost/0.wav")


def test_cleanup_is_idempotent(context: ServiceContext, clock: FakeClock) -> None:
    _produce(context, 2)
    clock.advance(hours=context.settings.retention_hours + 1)
    run_cleanup(context)
    second = run_cleanup(context)
    assert second.transcripts_deleted == 0
    assert second.recordings_deleted == 0
    assert second.audio_blobs_deleted == 0


def test_cleanup_keeps_fresh_data(context: ServiceContext) -> None:
    rid = _produce(context, 3)
    result = run_cleanup(context)
    assert result.recordings_deleted == 0
    assert context.recordings.get(rid) is not None

from voice_recorder.digest import compute_content_digest
from voice_recorder.domain import RecordingState
from voice_recorder.services.context import (
    ServiceContext,
    audio_path,
    raw_path,
    transcript_path,
)
from voice_recorder.services.recordings import (
    complete_recording,
    create_recording,
    get_recording,
    upload_chunk,
)

from .conftest import drain, unique_wav


def _run(ctx: ServiceContext, texts: list[str]) -> str:
    rec, _ = create_recording(
        ctx, client_recording_id="crid-pipe", refine_model="gpt-5.6-luna", language="cs"
    )
    rid = rec.recording_id
    for i, text in enumerate(texts):
        data = unique_wav(i)
        ctx.transcriber.responses[data] = text  # type: ignore[attr-defined]
        upload_chunk(
            ctx,
            recording_id=rid,
            index=i,
            data=data,
            checksum=compute_content_digest(data),
            duration_ms=30000,
            overlap_ms=1500,
            started_at=None,
        )
    complete_recording(ctx, recording_id=rid, chunk_count=len(texts))
    drain(ctx)
    return rid


def test_pipeline_completes_and_stitches(context: ServiceContext) -> None:
    rid = _run(context, ["the quick brown fox jumps", "fox jumps over the lazy dog"])
    rec = get_recording(context, rid)
    assert rec.state == RecordingState.COMPLETED
    transcript = context.transcripts.get(rec.transcript_id or "")
    assert transcript is not None
    body = context.blobs.get(
        context.settings.transcript_container, transcript_path(transcript.transcript_id)
    ).decode()
    # Overlap ("fox jumps") deduped exactly once.
    assert body == "the quick brown fox jumps over the lazy dog"
    assert body.count("fox jumps") == 1


def test_audio_deleted_after_transcription(context: ServiceContext) -> None:
    rid = _run(context, ["hello", "world"])
    assert not context.blobs.exists(context.settings.audio_container, audio_path(rid, 0))
    assert not context.blobs.exists(context.settings.audio_container, audio_path(rid, 1))


def test_raw_text_retained(context: ServiceContext) -> None:
    rid = _run(context, ["hello", "world"])
    assert context.blobs.exists(context.settings.raw_container, raw_path(rid))


def test_notification_carries_preview_not_body(context: ServiceContext) -> None:
    rid = _run(context, ["remember to call the doctor tomorrow morning"])
    rec = get_recording(context, rid)
    assert len(context.realtime.sent) == 1  # type: ignore[attr-defined]
    user_id, event = context.realtime.sent[0]  # type: ignore[attr-defined]
    assert user_id == context.user_id
    assert event.event == "transcript.completed"
    assert event.transcript_id == rec.transcript_id
    assert event.recording_id == rid
    assert event.preview
    # The event model intentionally has no full-body field.
    assert not hasattr(event, "body")
    assert "body" not in event.model_dump()


def test_transcriber_called_with_language_and_glossary(
    context: ServiceContext,
) -> None:
    _run(context, ["ahoj"])
    languages = {hints.language for hints in context.transcriber.calls}  # type: ignore[attr-defined]
    prompts = [hints.prompt for hints in context.transcriber.calls]  # type: ignore[attr-defined]
    phrase_lists = [hints.phrases for hints in context.transcriber.calls]  # type: ignore[attr-defined]
    assert languages == {"cs"}
    assert any("Azure" in p and "Entra" in p for p in prompts)
    assert any("Azure" in phrases and "Entra" in phrases for phrases in phrase_lists)


def test_refiner_uses_selected_deployment(context: ServiceContext) -> None:
    rec, _ = create_recording(
        context,
        client_recording_id="crid-alt",
        refine_model="gpt-5.6-terra",
        language="cs",
    )
    rid = rec.recording_id
    data = unique_wav(0)
    context.transcriber.responses[data] = "text"  # type: ignore[attr-defined]
    upload_chunk(
        context,
        recording_id=rid,
        index=0,
        data=data,
        checksum=compute_content_digest(data),
        duration_ms=30000,
        overlap_ms=1500,
        started_at=None,
    )
    complete_recording(context, recording_id=rid, chunk_count=1)
    drain(context)
    deployments = {dep for _, dep in context.refiner.calls}  # type: ignore[attr-defined]
    assert deployments == {"gpt-5.6-terra"}


def test_refiner_uses_gpt_6_luna_for_new_recordings(context: ServiceContext) -> None:
    rec, _ = create_recording(
        context,
        client_recording_id="crid-gpt6",
        refine_model="gpt-6-luna",
        language="cs",
    )
    data = unique_wav(0)
    context.transcriber.responses[data] = "ahoj světe"  # type: ignore[attr-defined]
    upload_chunk(
        context,
        recording_id=rec.recording_id,
        index=0,
        data=data,
        checksum=compute_content_digest(data),
        duration_ms=30000,
        overlap_ms=1500,
        started_at=None,
    )
    complete_recording(context, recording_id=rec.recording_id, chunk_count=1)
    drain(context)
    assert get_recording(context, rec.recording_id).state == RecordingState.COMPLETED
    assert context.refiner.calls == [("ahoj světe", "gpt-6-luna")]  # type: ignore[attr-defined]

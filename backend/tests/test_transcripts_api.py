from fastapi.testclient import TestClient

from voice_recorder.digest import compute_content_digest
from voice_recorder.services.context import ServiceContext
from voice_recorder.services.recordings import (
    complete_recording,
    create_recording,
    upload_chunk,
)

from .conftest import FakeClock, drain, unique_wav


def _produce_transcript(ctx: ServiceContext, seed: int, text: str) -> str:
    rec, _ = create_recording(
        ctx,
        client_recording_id=f"crid-{seed}",
        refine_model="gpt-5.6-luna",
        language="cs",
    )
    rid = rec.recording_id
    data = unique_wav(seed)
    ctx.transcriber.responses[data] = text  # type: ignore[attr-defined]
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
    updated = ctx.recordings.get(rid)
    assert updated is not None and updated.transcript_id is not None
    return updated.transcript_id


def test_get_transcript_returns_body(
    client: TestClient, context: ServiceContext, auth: dict[str, str]
) -> None:
    tid = _produce_transcript(context, 1, "remember to call the doctor")
    resp = client.get(f"/v1/transcripts/{tid}", headers=auth)
    assert resp.status_code == 200
    body = resp.json()
    assert body["transcript_id"] == tid
    assert body["body"]
    assert body["character_count"] == len(body["body"])
    assert body["refine_model"] == "gpt-5.6-luna"


def test_get_expired_transcript_404(
    client: TestClient, context: ServiceContext, clock: FakeClock, auth: dict[str, str]
) -> None:
    tid = _produce_transcript(context, 2, "short note")
    clock.advance(hours=context.settings.retention_hours + 1)
    resp = client.get(f"/v1/transcripts/{tid}", headers=auth)
    assert resp.status_code == 404


def test_list_pagination(
    client: TestClient, context: ServiceContext, clock: FakeClock, auth: dict[str, str]
) -> None:
    ids = []
    for i in range(3):
        ids.append(_produce_transcript(context, 10 + i, f"note number {i}"))
        clock.advance(seconds=5)

    first = client.get("/v1/transcripts?limit=2", headers=auth).json()
    assert len(first["items"]) == 2
    assert first["next_cursor"] is not None
    # Newest first: the last produced transcript appears first.
    assert first["items"][0]["transcript_id"] == ids[2]

    cursor = first["next_cursor"]
    second = client.get(f"/v1/transcripts?limit=2&cursor={cursor}", headers=auth).json()
    assert len(second["items"]) == 1
    assert second["next_cursor"] is None
    assert second["items"][0]["transcript_id"] == ids[0]


def test_list_only_returns_previews(
    client: TestClient, context: ServiceContext, auth: dict[str, str]
) -> None:
    _produce_transcript(context, 20, "a private thought that should not leak in list")
    page = client.get("/v1/transcripts", headers=auth).json()
    assert page["items"]
    for item in page["items"]:
        assert "body" not in item
        assert len(item["preview"]) <= 140


def test_get_unknown_transcript_404(client: TestClient, auth: dict[str, str]) -> None:
    import uuid

    resp = client.get(f"/v1/transcripts/{uuid.uuid4()}", headers=auth)
    assert resp.status_code == 404

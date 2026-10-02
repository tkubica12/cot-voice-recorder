import uuid

import pytest
from fastapi.testclient import TestClient

from .conftest import chunk_headers, make_wav


def _create_body(**extra: object) -> dict[str, object]:
    body: dict[str, object] = {"client_recording_id": str(uuid.uuid4())}
    body.update(extra)
    return body


def test_create_returns_201_with_location(client: TestClient, auth: dict[str, str]) -> None:
    body = _create_body(refine_model="gpt-5.6-terra", language="cs")
    resp = client.post("/v1/recordings", json=body, headers=auth)
    assert resp.status_code == 201
    data = resp.json()
    assert data["state"] == "recording"
    assert data["refine_model"] == "gpt-5.6-terra"
    assert data["progress"] == {
        "expected_chunk_count": None,
        "received_chunk_count": 0,
        "transcribed_chunk_count": 0,
        "streamed_audio_ms": 0,
        "stream_preview": "",
        "stream_attempt": 0,
        "stream_error": None,
    }
    assert resp.headers["Location"] == f"/v1/recordings/{data['recording_id']}"


def test_create_is_idempotent(client: TestClient, auth: dict[str, str]) -> None:
    body = _create_body()
    first = client.post("/v1/recordings", json=body, headers=auth)
    second = client.post("/v1/recordings", json=body, headers=auth)
    assert first.status_code == 201
    assert second.status_code == 200
    assert first.json()["recording_id"] == second.json()["recording_id"]


def test_create_defaults_model_and_language(client: TestClient, auth: dict[str, str]) -> None:
    resp = client.post("/v1/recordings", json=_create_body(), headers=auth)
    data = resp.json()
    assert data["refine_model"] == "gpt-6-luna"
    assert data["language"] == "cs"
    assert data["transcription_mode"] == "chunked"
    assert data["audio_layout"] == "legacy_overlap"
    assert data["refinement_enabled"] is True


def test_create_with_new_and_legacy_models(client: TestClient, auth: dict[str, str]) -> None:
    for model in ("gpt-6-luna", "gpt-5.6-luna", "gpt-5.6-terra"):
        resp = client.post("/v1/recordings", json=_create_body(refine_model=model), headers=auth)
        assert resp.status_code == 201
        assert resp.json()["refine_model"] == model
        rid = resp.json()["recording_id"]
        assert client.get(f"/v1/recordings/{rid}", headers=auth).json()["refine_model"] == model


def test_get_recording_returns_progress(client: TestClient, auth: dict[str, str]) -> None:
    rid = client.post("/v1/recordings", json=_create_body(), headers=auth).json()["recording_id"]
    resp = client.get(f"/v1/recordings/{rid}", headers=auth)
    assert resp.status_code == 200
    assert resp.json()["recording_id"] == rid


def test_get_unknown_recording_404(client: TestClient, auth: dict[str, str]) -> None:
    resp = client.get(f"/v1/recordings/{uuid.uuid4()}", headers=auth)
    assert resp.status_code == 404


def test_streaming_contract_is_immutable_and_upload_is_idempotent(client, auth, context):
    body = _create_body(
        transcription_mode="streaming",
        audio_layout="contiguous",
        refinement_enabled=False,
        language="auto",
    )
    rec = client.post("/v1/recordings", json=body, headers=auth).json()
    rid = rec["recording_id"]
    assert rec["transcription_mode"] == "streaming"
    assert rec["audio_layout"] == "contiguous"
    assert rec["refinement_enabled"] is False
    retry = client.post(
        "/v1/recordings",
        json={**body, "refinement_enabled": True, "audio_layout": "legacy_overlap"},
        headers=auth,
    )
    assert retry.status_code == 200
    assert retry.json()["refinement_enabled"] is False
    assert retry.json()["audio_layout"] == "contiguous"
    audio = make_wav()
    headers = {
        **auth,
        **chunk_headers(audio),
        "X-Audio-Start-Sample": "0",
        "X-Audio-Sample-Count": "320",
    }
    first = client.put(f"/v1/recordings/{rid}/chunks/0", content=audio, headers=headers)
    second = client.put(f"/v1/recordings/{rid}/chunks/0", content=audio, headers=headers)
    assert first.status_code == 202
    assert second.status_code == 200
    assert context.chunks.get(rid, 0).start_sample == 0
    assert context.chunks.get(rid, 0).sample_count == 320


@pytest.mark.parametrize(
    "sample_headers",
    [
        {},
        {"X-Audio-Start-Sample": "0"},
        {"X-Audio-Sample-Count": "320"},
        {"X-Audio-Start-Sample": "-1", "X-Audio-Sample-Count": "320"},
        {"X-Audio-Start-Sample": "1", "X-Audio-Sample-Count": "320"},
        {"X-Audio-Start-Sample": "0", "X-Audio-Sample-Count": "319"},
        {
            "X-Audio-Start-Sample": "0",
            "X-Audio-Sample-Count": "320",
            "X-Chunk-Overlap-Ms": "1500",
        },
    ],
)
def test_streaming_upload_requires_exact_sample_headers(client, auth, sample_headers, context):
    rid = client.post(
        "/v1/recordings",
        json=_create_body(transcription_mode="streaming", audio_layout="contiguous"),
        headers=auth,
    ).json()["recording_id"]
    audio = make_wav()
    response = client.put(
        f"/v1/recordings/{rid}/chunks/0",
        content=audio,
        headers={**auth, **chunk_headers(audio), **sample_headers},
    )
    assert response.status_code == 422
    assert context.chunks.get(rid, 0) is None

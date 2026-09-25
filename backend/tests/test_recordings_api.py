import uuid

from fastapi.testclient import TestClient


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

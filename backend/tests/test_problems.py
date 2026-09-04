import uuid

from fastapi.testclient import TestClient

from .conftest import chunk_headers, make_wav


def test_not_found_problem_shape(client: TestClient, auth: dict[str, str]) -> None:
    rid = str(uuid.uuid4())
    resp = client.get(f"/v1/recordings/{rid}", headers=auth)
    assert resp.status_code == 404
    assert resp.headers["content-type"].startswith("application/problem+json")
    body = resp.json()
    assert body["type"].endswith("/not-found")
    assert body["title"] == "Not found"
    assert body["status"] == 404
    assert body["code"] == "not-found"
    assert body["instance"] == f"/v1/recordings/{rid}"
    assert "trace_id" in body


def test_trace_id_header_present(client: TestClient) -> None:
    resp = client.get("/health/live")
    assert "X-Request-Id" in resp.headers


def test_incoming_trace_id_is_echoed(client: TestClient) -> None:
    resp = client.get("/health/live", headers={"X-Request-Id": "trace-abc"})
    assert resp.headers["X-Request-Id"] == "trace-abc"


def test_create_validation_error(client: TestClient, auth: dict[str, str]) -> None:
    resp = client.post("/v1/recordings", json={}, headers=auth)
    assert resp.status_code == 422
    body = resp.json()
    assert body["code"] == "validation"
    assert body["errors"]


def test_create_rejects_unknown_field(client: TestClient, auth: dict[str, str]) -> None:
    resp = client.post(
        "/v1/recordings",
        json={"client_recording_id": str(uuid.uuid4()), "surprise": 1},
        headers=auth,
    )
    assert resp.status_code == 422


def test_unknown_route_is_problem(client: TestClient) -> None:
    resp = client.get("/does-not-exist")
    assert resp.status_code == 404
    assert resp.headers["content-type"].startswith("application/problem+json")


def test_unsupported_media_type(client: TestClient, auth: dict[str, str]) -> None:
    rid = _create(client, auth)
    data = make_wav()
    resp = client.put(
        f"/v1/recordings/{rid}/chunks/0",
        content=data,
        headers={"Content-Type": "application/octet-stream", **_digest(data)},
    )
    resp = client.put(
        f"/v1/recordings/{rid}/chunks/0",
        content=data,
        headers={**auth, "Content-Type": "text/plain", **_digest(data)},
    )
    assert resp.status_code == 415


def test_payload_too_large(client: TestClient, auth: dict[str, str]) -> None:
    rid = _create(client, auth)
    big = make_wav(9 * 1024 * 1024)
    resp = client.put(
        f"/v1/recordings/{rid}/chunks/0",
        content=big,
        headers={**auth, **chunk_headers(big)},
    )
    assert resp.status_code == 413


def _digest(data: bytes) -> dict[str, str]:
    from voice_recorder.digest import compute_content_digest

    return {"Content-Digest": compute_content_digest(data)}


def _create(client: TestClient, auth: dict[str, str]) -> str:
    resp = client.post(
        "/v1/recordings",
        json={"client_recording_id": str(uuid.uuid4())},
        headers=auth,
    )
    assert resp.status_code == 201
    return resp.json()["recording_id"]

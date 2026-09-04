from fastapi.testclient import TestClient


def test_negotiate_returns_client_access(client: TestClient, auth: dict[str, str]) -> None:
    resp = client.post("/v1/realtime/negotiate", json={}, headers=auth)
    assert resp.status_code == 200
    body = resp.json()
    assert body["url"].startswith("wss://")
    assert body["hub"] == "transcripts"
    assert body["group"] == "user"
    assert "expires_at" in body


def test_negotiate_without_body(client: TestClient, auth: dict[str, str]) -> None:
    resp = client.post("/v1/realtime/negotiate", headers=auth)
    assert resp.status_code == 200


def test_negotiate_requires_auth(client: TestClient) -> None:
    resp = client.post("/v1/realtime/negotiate", json={})
    assert resp.status_code == 401

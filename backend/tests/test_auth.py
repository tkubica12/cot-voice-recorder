import pytest
from fastapi.testclient import TestClient

from voice_recorder.auth import (
    ForbiddenError,
    GoogleTokenVerifier,
    StaticTokenVerifier,
    UnauthorizedError,
)

from .conftest import FORBIDDEN_TOKEN, GOOD_TOKEN


def test_health_is_unauthenticated(client: TestClient) -> None:
    assert client.get("/health/live").status_code == 200
    assert client.get("/health/ready").status_code == 200


def test_missing_token_is_401(client: TestClient) -> None:
    resp = client.get("/v1/transcripts")
    assert resp.status_code == 401
    assert resp.headers["content-type"].startswith("application/problem+json")
    assert "WWW-Authenticate" in resp.headers


def test_non_bearer_scheme_is_401(client: TestClient) -> None:
    resp = client.get("/v1/transcripts", headers={"Authorization": "Basic abc"})
    assert resp.status_code == 401


def test_unknown_token_is_401(client: TestClient) -> None:
    resp = client.get("/v1/transcripts", headers={"Authorization": "Bearer nope"})
    assert resp.status_code == 401


def test_forbidden_token_is_403(client: TestClient) -> None:
    resp = client.get("/v1/transcripts", headers={"Authorization": f"Bearer {FORBIDDEN_TOKEN}"})
    assert resp.status_code == 403
    assert resp.json()["code"] == "forbidden"


def test_good_token_is_allowed(client: TestClient, auth: dict[str, str]) -> None:
    resp = client.get("/v1/transcripts", headers=auth)
    assert resp.status_code == 200


def test_static_verifier_outcomes() -> None:
    verifier = StaticTokenVerifier(
        valid_tokens={GOOD_TOKEN: object()},  # type: ignore[dict-item]
        forbidden_tokens=frozenset({FORBIDDEN_TOKEN}),
    )
    with pytest.raises(ForbiddenError):
        verifier.verify(FORBIDDEN_TOKEN)
    with pytest.raises(UnauthorizedError):
        verifier.verify("unknown")


def test_google_verifier_requires_configuration() -> None:
    with pytest.raises(ValueError, match="allowed_audiences"):
        GoogleTokenVerifier(
            jwks_uri="https://example/jwks",
            allowed_audiences=(),
            allowed_issuers=("https://accounts.google.com",),
            allowlisted_email="a@b.com",
        )
    with pytest.raises(ValueError, match="allowlisted_email"):
        GoogleTokenVerifier(
            jwks_uri="https://example/jwks",
            allowed_audiences=("aud",),
            allowed_issuers=("https://accounts.google.com",),
            allowlisted_email="",
        )

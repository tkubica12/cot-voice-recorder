"""Cryptographic tests for the real Google ID-token verifier.

We generate an RSA keypair, sign tokens with RS256, and stub only the JWKS lookup so
the actual signature/claim validation in :class:`GoogleTokenVerifier` is exercised.
"""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from typing import Any

import jwt
import pytest
from cryptography.hazmat.primitives.asymmetric import rsa

from voice_recorder.auth import GoogleTokenVerifier
from voice_recorder.problems import ForbiddenError, UnauthorizedError

ISSUER = "https://accounts.google.com"
AUDIENCE = "android.apps.googleusercontent.com"
EMAIL = "owner@example.com"

_private_key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
_public_key = _private_key.public_key()


class _FakeSigningKey:
    key = _public_key


def _make_verifier() -> GoogleTokenVerifier:
    verifier = GoogleTokenVerifier(
        jwks_uri="https://example/jwks",
        allowed_audiences=(AUDIENCE,),
        allowed_issuers=(ISSUER,),
        allowlisted_email=EMAIL,
    )
    verifier._jwks_client.get_signing_key_from_jwt = (  # type: ignore[method-assign]
        lambda _token: _FakeSigningKey()
    )
    return verifier


def _token(**overrides: Any) -> str:
    now = datetime.now(UTC)
    claims: dict[str, Any] = {
        "iss": ISSUER,
        "aud": AUDIENCE,
        "sub": "12345",
        "email": EMAIL,
        "email_verified": True,
        "iat": now,
        "exp": now + timedelta(hours=1),
    }
    claims.update(overrides)
    return jwt.encode(claims, _private_key, algorithm="RS256")


def test_valid_token_accepted() -> None:
    user = _make_verifier().verify(_token())
    assert user.email == EMAIL
    assert user.subject == "12345"


def test_expired_token_rejected() -> None:
    past = datetime.now(UTC) - timedelta(hours=2)
    with pytest.raises(UnauthorizedError):
        _make_verifier().verify(_token(iat=past, exp=past + timedelta(minutes=1)))


def test_wrong_audience_rejected() -> None:
    with pytest.raises(UnauthorizedError):
        _make_verifier().verify(_token(aud="someone-else"))


def test_wrong_issuer_rejected() -> None:
    with pytest.raises(UnauthorizedError):
        _make_verifier().verify(_token(iss="https://evil.example"))


def test_unverified_email_forbidden() -> None:
    with pytest.raises(ForbiddenError):
        _make_verifier().verify(_token(email_verified=False))


def test_non_allowlisted_email_forbidden() -> None:
    with pytest.raises(ForbiddenError):
        _make_verifier().verify(_token(email="intruder@example.com"))


def test_hs256_token_rejected() -> None:
    # A token signed with a different algorithm must not validate against RS256.
    token = jwt.encode({"iss": ISSUER, "aud": AUDIENCE}, "secret", algorithm="HS256")
    with pytest.raises(UnauthorizedError):
        _make_verifier().verify(token)


def test_missing_email_rejected() -> None:
    with pytest.raises(UnauthorizedError):
        _make_verifier().verify(_token(email=None))

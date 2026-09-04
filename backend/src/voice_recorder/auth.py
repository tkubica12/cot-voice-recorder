"""Google OpenID Connect ID-token authentication.

Validation is intentionally strict: RS256 only, signature verified against Google's
JWKS (cached by PyJWT), a configured issuer set, ``exp``/``iat`` presence and validity,
an allowed audience set, a verified email, and a single-email allowlist.

The concrete verifier is hidden behind :class:`TokenVerifier` so tests can substitute a
deterministic fake without touching Google.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Protocol

import jwt
from jwt import PyJWKClient

from .problems import ForbiddenError, UnauthorizedError


@dataclass(frozen=True, slots=True)
class AuthenticatedUser:
    email: str
    subject: str
    audience: str


class TokenVerifier(Protocol):
    def verify(self, token: str) -> AuthenticatedUser:
        """Return the authenticated user or raise Unauthorized/Forbidden."""
        ...


class GoogleTokenVerifier:
    """Cryptographic Google ID-token verifier backed by PyJWT + cached JWKS."""

    def __init__(
        self,
        *,
        jwks_uri: str,
        allowed_audiences: tuple[str, ...],
        allowed_issuers: tuple[str, ...],
        allowlisted_email: str,
    ) -> None:
        if not allowed_audiences:
            raise ValueError("allowed_audiences must be configured")
        if not allowlisted_email:
            raise ValueError("allowlisted_email must be configured")
        self._audiences = list(allowed_audiences)
        self._issuers = set(allowed_issuers)
        self._allowlisted_email = allowlisted_email.strip().lower()
        # PyJWKClient caches keys in-process and refreshes on unknown kid.
        self._jwks_client = PyJWKClient(jwks_uri, cache_keys=True)

    def verify(self, token: str) -> AuthenticatedUser:
        try:
            signing_key = self._jwks_client.get_signing_key_from_jwt(token)
            claims = jwt.decode(
                token,
                signing_key.key,
                algorithms=["RS256"],
                audience=self._audiences,
                options={
                    "require": ["exp", "iat", "aud", "iss"],
                    "verify_signature": True,
                    "verify_exp": True,
                    "verify_iat": True,
                    "verify_aud": True,
                },
            )
        except jwt.PyJWTError as exc:
            raise UnauthorizedError(f"ID token is invalid: {exc}") from exc
        except Exception as exc:  # JWKS fetch / network / key errors
            raise UnauthorizedError("Unable to verify the ID token signing key.") from exc

        issuer = claims.get("iss")
        if issuer not in self._issuers:
            raise UnauthorizedError("ID token issuer is not accepted.")

        email = claims.get("email")
        if not email:
            raise UnauthorizedError("ID token does not contain an email claim.")

        if claims.get("email_verified") is not True:
            raise ForbiddenError("This account's email is not verified.")
        if str(email).strip().lower() != self._allowlisted_email:
            raise ForbiddenError("This account is not permitted to use this service.")

        return AuthenticatedUser(
            email=str(email),
            subject=str(claims.get("sub", "")),
            audience=str(claims.get("aud", "")),
        )


class StaticTokenVerifier:
    """Deterministic verifier for tests/local mode.

    Maps opaque bearer strings to fixed outcomes. Never use in production.
    """

    def __init__(
        self,
        *,
        valid_tokens: dict[str, AuthenticatedUser] | None = None,
        forbidden_tokens: frozenset[str] = frozenset(),
    ) -> None:
        self._valid = valid_tokens or {}
        self._forbidden = forbidden_tokens

    def verify(self, token: str) -> AuthenticatedUser:
        if token in self._forbidden:
            raise ForbiddenError("This account is not permitted to use this service.")
        user = self._valid.get(token)
        if user is None:
            raise UnauthorizedError("Unknown or invalid token.")
        return user

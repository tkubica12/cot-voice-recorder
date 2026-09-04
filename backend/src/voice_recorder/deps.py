"""FastAPI dependencies: service context, token verifier, and auth guard."""

from __future__ import annotations

from fastapi import Request

from .auth import AuthenticatedUser, TokenVerifier
from .problems import UnauthorizedError
from .services.context import ServiceContext


def get_context(request: Request) -> ServiceContext:
    return request.app.state.context  # type: ignore[no-any-return]


def get_verifier(request: Request) -> TokenVerifier:
    return request.app.state.verifier  # type: ignore[no-any-return]


def require_user(request: Request) -> AuthenticatedUser:
    """Validate the Google ID token on the incoming request or raise 401/403."""
    authorization = request.headers.get("Authorization")
    if not authorization:
        raise UnauthorizedError("Missing Authorization header.", error="invalid_request")
    scheme, _, token = authorization.partition(" ")
    if scheme.lower() != "bearer" or not token.strip():
        raise UnauthorizedError("Authorization header must be a Bearer token.")
    verifier: TokenVerifier = request.app.state.verifier
    return verifier.verify(token.strip())

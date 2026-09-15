"""RFC 9457 Problem Details, with stable problem types and error codes.

Every API error is rendered as ``application/problem+json`` with a stable ``type`` URI,
a ``title``, the HTTP ``status``, a human-readable ``detail``, the ``instance`` (request
path), a machine-readable ``code``, and the request ``trace_id``.
"""

from __future__ import annotations

from typing import Any

PROBLEM_BASE = "https://voice-recorder/errors/"


class ProblemError(Exception):
    """Base class for errors that render as RFC 9457 Problem Details."""

    status: int = 500
    code: str = "internal-error"
    title: str = "Internal server error"

    def __init__(
        self,
        detail: str | None = None,
        *,
        errors: list[dict[str, str]] | None = None,
        headers: dict[str, str] | None = None,
    ) -> None:
        self.detail = detail or self.title
        self.errors = errors
        self.headers = headers or {}
        super().__init__(self.detail)

    @property
    def type_uri(self) -> str:
        return f"{PROBLEM_BASE}{self.code}"

    def to_problem(self, instance: str, trace_id: str | None) -> dict[str, Any]:
        problem: dict[str, Any] = {
            "type": self.type_uri,
            "title": self.title,
            "status": self.status,
            "detail": self.detail,
            "instance": instance,
            "code": self.code,
        }
        if self.errors:
            problem["errors"] = self.errors
        if trace_id is not None:
            problem["trace_id"] = trace_id
        return problem


class BadRequestError(ProblemError):
    status = 400
    code = "bad-request"
    title = "Bad request"


class UnauthorizedError(ProblemError):
    status = 401
    code = "unauthorized"
    title = "Unauthorized"

    def __init__(self, detail: str | None = None, *, error: str = "invalid_token") -> None:
        headers = {"WWW-Authenticate": f'Bearer error="{error}"'}
        super().__init__(detail, headers=headers)


class ForbiddenError(ProblemError):
    status = 403
    code = "forbidden"
    title = "Forbidden"


class NotFoundError(ProblemError):
    status = 404
    code = "not-found"
    title = "Not found"


class ChunkConflictError(ProblemError):
    status = 409
    code = "chunk-conflict"
    title = "Chunk conflict"


class CompleteConflictError(ProblemError):
    status = 409
    code = "complete-conflict"
    title = "Complete conflict"


class PayloadTooLargeError(ProblemError):
    status = 413
    code = "payload-too-large"
    title = "Payload too large"


class UnsupportedMediaTypeError(ProblemError):
    status = 415
    code = "unsupported-media-type"
    title = "Unsupported media type"


class ValidationProblemError(ProblemError):
    status = 422
    code = "validation"
    title = "Validation error"


class ServiceUnavailableError(ProblemError):
    status = 503
    code = "service-unavailable"
    title = "Service unavailable"


class TooManyRequestsError(ProblemError):
    status = 429
    code = "too-many-requests"
    title = "Too many requests"


class BadGatewayError(ProblemError):
    status = 502
    code = "bad-gateway"
    title = "Bad gateway"


class GatewayTimeoutError(ProblemError):
    status = 504
    code = "gateway-timeout"
    title = "Gateway timeout"

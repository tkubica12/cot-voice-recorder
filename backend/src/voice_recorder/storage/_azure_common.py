"""Shared helpers for Azure SDK adapters."""

from __future__ import annotations

from azure.core.exceptions import (
    HttpResponseError,
    ServiceRequestError,
    ServiceResponseError,
)

from ..errors import TransientError

_TRANSIENT_STATUS = frozenset({408, 429, 500, 502, 503, 504})


def is_transient(exc: BaseException) -> bool:
    if isinstance(exc, ServiceRequestError | ServiceResponseError):
        return True
    if isinstance(exc, HttpResponseError):
        return exc.status_code in _TRANSIENT_STATUS
    return False


def wrap_transient(exc: BaseException, message: str) -> Exception:
    """Return a TransientError for retryable Azure failures, else the original."""
    if is_transient(exc):
        return TransientError(f"{message}: {exc}")
    return exc if isinstance(exc, Exception) else RuntimeError(str(exc))

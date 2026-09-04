"""RFC 9530 ``Content-Digest`` parsing and SHA-256 verification.

Header form (single algorithm, sha-256): ``sha-256=:<base64(digest)>:``
"""

from __future__ import annotations

import base64
import binascii
import hashlib
import re

from .problems import BadRequestError, ValidationProblemError

_DIGEST_RE = re.compile(r"^sha-256=:(?P<b64>[A-Za-z0-9+/=]+):$")


def parse_content_digest(header: str | None) -> str:
    """Return the canonical ``sha-256=:<base64>:`` digest or raise a 4xx problem.

    Raises:
        BadRequestError: when the header is missing.
        ValidationProblemError: when present but malformed.
    """
    if header is None or not header.strip():
        raise BadRequestError("Content-Digest header is required.")
    match = _DIGEST_RE.match(header.strip())
    if match is None:
        raise ValidationProblemError(
            "Content-Digest must be of the form sha-256=:<base64>:.",
            errors=[{"field": "Content-Digest", "message": "malformed sha-256 digest"}],
        )
    b64 = match.group("b64")
    try:
        raw = base64.b64decode(b64, validate=True)
    except (binascii.Error, ValueError) as exc:
        raise ValidationProblemError(
            "Content-Digest base64 payload is invalid.",
            errors=[{"field": "Content-Digest", "message": "invalid base64"}],
        ) from exc
    if len(raw) != 32:
        raise ValidationProblemError(
            "Content-Digest must be a 32-byte SHA-256 value.",
            errors=[{"field": "Content-Digest", "message": "expected 32-byte digest"}],
        )
    # Re-encode canonically so echoes/comparisons are stable regardless of padding.
    return f"sha-256=:{base64.b64encode(raw).decode('ascii')}:"


def compute_content_digest(data: bytes) -> str:
    """Compute the canonical ``Content-Digest`` value for ``data``."""
    raw = hashlib.sha256(data).digest()
    return f"sha-256=:{base64.b64encode(raw).decode('ascii')}:"


def verify_content_digest(data: bytes, expected: str) -> bool:
    """Constant-time comparison of the computed digest against ``expected``."""
    import hmac

    return hmac.compare_digest(compute_content_digest(data), expected)

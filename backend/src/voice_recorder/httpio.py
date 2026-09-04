"""HTTP helpers for bounded request-body reading."""

from __future__ import annotations

from fastapi import Request

from .problems import PayloadTooLargeError


async def read_bounded_body(request: Request, max_bytes: int) -> bytes:
    """Read the request body, enforcing ``max_bytes`` without unbounded buffering.

    Rejects early on a too-large ``Content-Length``, and stops as soon as the streamed
    bytes exceed the limit so memory stays bounded to roughly ``max_bytes``.
    """
    content_length = request.headers.get("content-length")
    if content_length and content_length.isdigit() and int(content_length) > max_bytes:
        raise PayloadTooLargeError(f"Chunk exceeds the {max_bytes} byte limit.")
    buffer = bytearray()
    async for chunk in request.stream():
        buffer.extend(chunk)
        if len(buffer) > max_bytes:
            raise PayloadTooLargeError(f"Chunk exceeds the {max_bytes} byte limit.")
    return bytes(buffer)

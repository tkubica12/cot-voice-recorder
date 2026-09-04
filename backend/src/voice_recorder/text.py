"""Small text utilities."""

from __future__ import annotations


def make_preview(body: str, *, max_chars: int = 140) -> str:
    """Return a single-line preview, truncated with an ellipsis if needed."""
    collapsed = " ".join(body.split())
    if len(collapsed) <= max_chars:
        return collapsed
    return collapsed[: max_chars - 1].rstrip() + "\u2026"

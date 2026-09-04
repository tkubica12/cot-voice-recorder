"""Deterministic overlap stitching of per-chunk transcripts.

Chunks are captured with a fixed overlap (nominally 1.5 s), so consecutive chunk
transcripts usually repeat a few words at the seam. We remove that duplication by
matching the token suffix of the accumulated text against the token prefix of the next
chunk, tolerant of punctuation and case, before the text is refined.
"""

from __future__ import annotations

import re
import unicodedata

_STRIP_RE = re.compile(r"[^\w]", re.UNICODE)


def _normalize_token(token: str) -> str:
    """Lowercase, strip accents-insensitively, and drop surrounding punctuation."""
    folded = unicodedata.normalize("NFKC", token).casefold()
    return _STRIP_RE.sub("", folded)


def _dedupe_head(
    accumulated: list[str],
    incoming: list[str],
    *,
    min_overlap_chars: int,
    max_overlap_tokens: int,
) -> list[str]:
    """Return ``incoming`` with any leading tokens that overlap ``accumulated`` removed."""
    limit = min(len(accumulated), len(incoming), max_overlap_tokens)
    for k in range(limit, 0, -1):
        tail = [_normalize_token(t) for t in accumulated[-k:]]
        head = [_normalize_token(t) for t in incoming[:k]]
        if tail != head:
            continue
        if not any(tail):
            continue
        # Avoid merging on a single short stop-word, which is often coincidental.
        matched_chars = sum(len(t) for t in tail)
        if k < 2 and matched_chars < min_overlap_chars:
            continue
        return incoming[k:]
    return incoming


def stitch_chunks(
    texts: list[str],
    *,
    min_overlap_chars: int = 8,
    max_overlap_tokens: int = 60,
) -> str:
    """Join per-chunk transcripts into one string, removing overlap duplication."""
    result: list[str] = []
    for text in texts:
        tokens = text.split()
        if not tokens:
            continue
        if not result:
            result = tokens
            continue
        remainder = _dedupe_head(
            result,
            tokens,
            min_overlap_chars=min_overlap_chars,
            max_overlap_tokens=max_overlap_tokens,
        )
        result.extend(remainder)
    return " ".join(result)

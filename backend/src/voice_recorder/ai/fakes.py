"""Deterministic AI fakes for tests and local mode."""

from __future__ import annotations

from ..stitch import _normalize_token
from .protocols import TranscriptionHints


class FakeTranscriber:
    """Returns canned text per (recording context) or echoes a marker.

    Configure ``responses`` to map audio bytes to text; otherwise a deterministic
    placeholder derived from the byte length is returned.
    """

    def __init__(self, responses: dict[bytes, str] | None = None) -> None:
        self.responses = responses or {}
        self.calls: list[TranscriptionHints] = []

    def transcribe(self, audio: bytes, *, hints: TranscriptionHints) -> str:
        self.calls.append(hints)
        if audio in self.responses:
            return self.responses[audio]
        return f"chunk-{len(audio)}"


class FakeRefiner:
    """Refiner that lowercases and collapses whitespace, preserving word order.

    This mimics 'clean up without changing meaning' well enough for tests to assert
    the pipeline wiring and that no summarization occurs.
    """

    def __init__(self, mapping: dict[str, str] | None = None) -> None:
        self.mapping = mapping or {}
        self.calls: list[tuple[str, str]] = []

    def refine(self, raw_text: str, *, deployment: str) -> str:
        self.calls.append((raw_text, deployment))
        if raw_text in self.mapping:
            return self.mapping[raw_text]
        tokens = [t for t in (_normalize_token(w) for w in raw_text.split()) if t]
        return " ".join(tokens)

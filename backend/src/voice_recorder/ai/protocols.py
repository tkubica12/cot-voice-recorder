"""AI adapter protocols and error types."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Protocol

from ..errors import TerminalError, TransientError


class TranscriptionError(TransientError):
    """Retryable transcription failure."""


class TerminalTranscriptionError(TerminalError):
    """Non-retryable transcription failure (e.g., rejected input)."""


class RefinementError(TransientError):
    """Retryable refinement failure."""


class TerminalRefinementError(TerminalError):
    """Non-retryable refinement failure."""


@dataclass(frozen=True)
class TranscriptionHints:
    """Provider-neutral language and vocabulary hints for one audio chunk."""

    language: str
    prompt: str
    phrases: tuple[str, ...]


class Transcriber(Protocol):
    def transcribe(self, audio: bytes, *, hints: TranscriptionHints) -> str: ...


class Refiner(Protocol):
    def refine(self, raw_text: str, *, deployment: str) -> str: ...


class DictationRefiner(Protocol):
    def propose_edits(self, text: str, *, previous_text: str) -> str: ...

    def close(self) -> None: ...

"""AI adapter protocols and error types."""

from __future__ import annotations

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


class Transcriber(Protocol):
    def transcribe(self, audio: bytes, *, language: str, prompt: str) -> str: ...


class Refiner(Protocol):
    def refine(self, raw_text: str, *, deployment: str) -> str: ...

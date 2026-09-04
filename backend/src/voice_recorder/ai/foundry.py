"""Azure AI Foundry (Azure OpenAI) adapters using Entra token auth only.

Both adapters authenticate with a bearer-token provider backed by managed identity
(:class:`DefaultAzureCredential`); no API key is ever read.
"""

from __future__ import annotations

from typing import Any

import openai

from ..prompts import build_refinement_messages
from .protocols import (
    RefinementError,
    TerminalRefinementError,
    TerminalTranscriptionError,
    TranscriptionError,
)

_TRANSIENT_STATUS = frozenset({408, 409, 429, 500, 502, 503, 504})


def _is_transient(exc: openai.APIError) -> bool:
    if isinstance(
        exc,
        openai.APIConnectionError | openai.APITimeoutError | openai.RateLimitError,
    ):
        return True
    status = getattr(exc, "status_code", None)
    return status in _TRANSIENT_STATUS


class FoundryTranscriber:
    def __init__(self, client: Any, deployment: str) -> None:
        self._client = client
        self._deployment = deployment

    def transcribe(self, audio: bytes, *, language: str, prompt: str) -> str:
        try:
            result = self._client.audio.transcriptions.create(
                model=self._deployment,
                file=("chunk.wav", audio, "audio/wav"),
                language=language,
                prompt=prompt,
                response_format="text",
            )
        except openai.APIError as exc:
            if _is_transient(exc):
                raise TranscriptionError(str(exc)) from exc
            raise TerminalTranscriptionError(str(exc)) from exc
        return result if isinstance(result, str) else str(getattr(result, "text", result))


class FoundryRefiner:
    def __init__(self, client: Any) -> None:
        self._client = client

    def refine(self, raw_text: str, *, deployment: str) -> str:
        try:
            completion = self._client.chat.completions.create(
                model=deployment,
                messages=build_refinement_messages(raw_text),
            )
        except openai.APIError as exc:
            if _is_transient(exc):
                raise RefinementError(str(exc)) from exc
            raise TerminalRefinementError(str(exc)) from exc
        content = completion.choices[0].message.content
        if content is None:
            raise TerminalRefinementError("refinement returned empty content")
        return content if isinstance(content, str) else str(content)

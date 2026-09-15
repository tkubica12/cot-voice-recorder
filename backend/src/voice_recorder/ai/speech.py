"""Azure Speech Fast Transcription adapter for MAI-Transcribe models."""

from __future__ import annotations

import json
from typing import Any

import httpx
from azure.core.credentials import TokenCredential
from azure.core.exceptions import ClientAuthenticationError

from .protocols import (
    TerminalTranscriptionError,
    TranscriptionError,
    TranscriptionHints,
)

_TRANSIENT_STATUS = frozenset({408, 409, 429, 500, 502, 503, 504})
_SPEECH_SCOPE = "https://cognitiveservices.azure.com/.default"


class AzureSpeechTranscriber:
    """Invoke Azure Speech enhanced mode with Entra authentication."""

    def __init__(
        self,
        endpoint: str,
        credential: TokenCredential,
        *,
        model: str,
        api_version: str,
        transcribe_style: str,
        timeout_seconds: float,
        client: httpx.Client | None = None,
        strict_response: bool = False,
    ) -> None:
        self._url = f"{endpoint.rstrip('/')}/speechtotext/transcriptions:transcribe"
        self._credential = credential
        self._model = model
        self._api_version = api_version
        self._transcribe_style = transcribe_style
        self._timeout = timeout_seconds
        self._client = client or httpx.Client()
        self._strict_response = strict_response

    def close(self) -> None:
        self._client.close()

    def transcribe(self, audio: bytes, *, hints: TranscriptionHints) -> str:
        definition: dict[str, Any] = {
            "enhancedMode": {
                "enabled": True,
                "model": self._model,
                "modelOptions": {"transcribeStyle": self._transcribe_style},
            },
        }
        if hints.language != "auto":
            definition["locales"] = [hints.language]
        if hints.phrases:
            definition["phraseList"] = {"phrases": list(hints.phrases)}

        try:
            token = self._credential.get_token(_SPEECH_SCOPE).token
            response = self._client.post(
                self._url,
                params={"api-version": self._api_version},
                headers={"Authorization": f"Bearer {token}"},
                files={
                    "audio": ("chunk.wav", audio, "audio/wav"),
                    "definition": (
                        None,
                        json.dumps(definition, ensure_ascii=False),
                        "application/json",
                    ),
                },
                timeout=self._timeout,
            )
        except (httpx.TimeoutException, httpx.TransportError, ClientAuthenticationError) as exc:
            raise TranscriptionError(str(exc)) from exc

        if response.status_code >= 400 or (self._strict_response and not response.is_success):
            message = f"Azure Speech returned HTTP {response.status_code}: {response.text[:500]}"
            if response.status_code in _TRANSIENT_STATUS:
                raise TranscriptionError(message)
            raise TerminalTranscriptionError(message)

        try:
            payload = response.json()
            combined = payload["combinedPhrases"]
            if not isinstance(combined, list):
                raise TypeError("combinedPhrases must be a list")
            if self._strict_response and any(
                not isinstance(phrase, dict) or not isinstance(phrase.get("text"), str)
                for phrase in combined
            ):
                raise TypeError("combinedPhrases entries must contain text")
            texts = [
                phrase["text"].strip()
                for phrase in combined
                if isinstance(phrase, dict)
                and isinstance(phrase.get("text"), str)
                and phrase["text"].strip()
            ]
        except (json.JSONDecodeError, KeyError, TypeError) as exc:
            raise TerminalTranscriptionError("Azure Speech returned a malformed response") from exc

        return " ".join(texts)

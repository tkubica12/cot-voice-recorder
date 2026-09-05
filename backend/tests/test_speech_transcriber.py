from __future__ import annotations

import json

import httpx
import pytest
from azure.core.credentials import AccessToken
from azure.core.exceptions import ClientAuthenticationError

from voice_recorder.ai.protocols import (
    TerminalTranscriptionError,
    TranscriptionError,
    TranscriptionHints,
)
from voice_recorder.ai.speech import AzureSpeechTranscriber


class _Credential:
    def __init__(self, exc: Exception | None = None) -> None:
        self.exc = exc
        self.scopes: list[str] = []

    def get_token(self, *scopes: str, **kwargs: object) -> AccessToken:
        self.scopes.extend(scopes)
        if self.exc is not None:
            raise self.exc
        return AccessToken("test-token", 2_000_000_000)


def _hints() -> TranscriptionHints:
    return TranscriptionHints(
        language="cs",
        prompt="Domain glossary: Azure, Entra.",
        phrases=("Azure", "Entra", "Container Apps"),
    )


def _transcriber(
    handler: httpx.MockTransport,
    *,
    credential: _Credential | None = None,
) -> AzureSpeechTranscriber:
    return AzureSpeechTranscriber(
        "https://speech.example.test/",
        credential or _Credential(),  # type: ignore[arg-type]
        model="MAI-Transcribe-2",
        api_version="2025-10-15",
        transcribe_style="verbatim",
        timeout_seconds=180,
        client=httpx.Client(transport=handler),
    )


def test_sends_mai_definition_with_entra_auth_and_phrase_list() -> None:
    credential = _Credential()

    def handle(request: httpx.Request) -> httpx.Response:
        assert str(request.url) == (
            "https://speech.example.test/speechtotext/"
            "transcriptions:transcribe?api-version=2025-10-15"
        )
        assert request.headers["Authorization"] == "Bearer test-token"
        body = request.content.decode()
        assert 'name="audio"; filename="chunk.wav"' in body
        assert b"RIFF" in request.content
        assert 'name="definition"' in body
        assert '"model": "MAI-Transcribe-2"' in body
        assert '"transcribeStyle": "verbatim"' in body
        assert '"locales": ["cs"]' in body
        assert '"phrases": ["Azure", "Entra", "Container Apps"]' in body
        return httpx.Response(
            200,
            json={"combinedPhrases": [{"channel": 0, "text": "Ahoj z Azure."}]},
        )

    result = _transcriber(httpx.MockTransport(handle), credential=credential).transcribe(
        b"RIFF-audio", hints=_hints()
    )

    assert result == "Ahoj z Azure."
    assert credential.scopes == ["https://cognitiveservices.azure.com/.default"]


@pytest.mark.parametrize("status", [408, 409, 429, 500, 502, 503, 504])
def test_retryable_http_statuses_raise_transient_error(status: int) -> None:
    transport = httpx.MockTransport(
        lambda _: httpx.Response(status, json={"error": {"message": "retry"}})
    )
    with pytest.raises(TranscriptionError, match=f"HTTP {status}"):
        _transcriber(transport).transcribe(b"audio", hints=_hints())


@pytest.mark.parametrize("status", [400, 401, 403, 404, 413, 422])
def test_rejected_requests_raise_terminal_error(status: int) -> None:
    transport = httpx.MockTransport(
        lambda _: httpx.Response(status, json={"error": {"message": "rejected"}})
    )
    with pytest.raises(TerminalTranscriptionError, match=f"HTTP {status}"):
        _transcriber(transport).transcribe(b"audio", hints=_hints())


@pytest.mark.parametrize(
    ("response", "message"),
    [
        (httpx.Response(200, text="not-json"), "malformed"),
        (httpx.Response(200, json={}), "malformed"),
        (httpx.Response(200, json={"combinedPhrases": {}}), "malformed"),
    ],
)
def test_invalid_success_responses_are_terminal(response: httpx.Response, message: str) -> None:
    transport = httpx.MockTransport(lambda _: response)
    with pytest.raises(TerminalTranscriptionError, match=message):
        _transcriber(transport).transcribe(b"audio", hints=_hints())


@pytest.mark.parametrize(
    "payload",
    [
        {"combinedPhrases": []},
        {"combinedPhrases": [{"text": "  "}]},
    ],
)
def test_silent_audio_returns_empty_transcript(payload: dict[str, object]) -> None:
    transport = httpx.MockTransport(lambda _: httpx.Response(200, json=payload))
    assert _transcriber(transport).transcribe(b"audio", hints=_hints()) == ""


def test_transport_and_token_failures_are_retryable() -> None:
    transport = httpx.MockTransport(
        lambda request: (_ for _ in ()).throw(httpx.ConnectError("offline", request=request))
    )
    with pytest.raises(TranscriptionError, match="offline"):
        _transcriber(transport).transcribe(b"audio", hints=_hints())

    auth_failure = _Credential(ClientAuthenticationError("identity unavailable"))
    with pytest.raises(TranscriptionError, match="identity unavailable"):
        transcriber = _transcriber(
            httpx.MockTransport(lambda _: httpx.Response(200)),
            credential=auth_failure,
        )
        transcriber.transcribe(b"audio", hints=_hints())


def test_multiple_combined_phrases_are_joined() -> None:
    payload = {
        "combinedPhrases": [
            {"channel": 0, "text": "První část."},
            {"channel": 1, "text": "Druhá část."},
        ]
    }
    transport = httpx.MockTransport(lambda _: httpx.Response(200, content=json.dumps(payload)))
    assert _transcriber(transport).transcribe(b"audio", hints=_hints()) == (
        "První část. Druhá část."
    )

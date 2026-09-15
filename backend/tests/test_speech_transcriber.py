from __future__ import annotations

import json
from email import policy
from email.parser import BytesParser
from unittest.mock import Mock

import httpx
import pytest
from azure.core.credentials import AccessToken
from azure.core.exceptions import ClientAuthenticationError
from fastapi.testclient import TestClient

from voice_recorder.ai.protocols import (
    TerminalTranscriptionError,
    TranscriptionError,
    TranscriptionHints,
)
from voice_recorder.ai.speech import AzureSpeechTranscriber
from voice_recorder.app import create_app
from voice_recorder.auth import StaticTokenVerifier
from voice_recorder.bootstrap import _build_ai, build_dictation_transcriber
from voice_recorder.config import Settings
from voice_recorder.services.context import ServiceContext

from .conftest import make_wav


def _definition(request: httpx.Request) -> dict[str, object]:
    message = BytesParser(policy=policy.default).parsebytes(
        f"Content-Type: {request.headers['content-type']}\r\n\r\n".encode() + request.content
    )
    parts = [
        part
        for part in message.walk()
        if part.get_param("name", header="content-disposition") == "definition"
    ]
    assert len(parts) == 1
    payload = parts[0].get_payload(decode=True)
    assert isinstance(payload, bytes)
    return json.loads(payload)


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
    dictation: bool = False,
) -> AzureSpeechTranscriber:
    return AzureSpeechTranscriber(
        "https://speech.example.test/",
        credential or _Credential(),  # type: ignore[arg-type]
        model="MAI-Transcribe-2",
        api_version="2025-10-15",
        transcribe_style="clean" if dictation else "verbatim",
        timeout_seconds=20 if dictation else 180,
        client=httpx.Client(transport=handler),
        strict_response=dictation,
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


@pytest.mark.parametrize("language", ["auto", "cs", "en"])
def test_dictation_uses_clean_style_and_optional_locales(language: str) -> None:
    calls = 0

    def handle(request: httpx.Request) -> httpx.Response:
        nonlocal calls
        calls += 1
        body = request.content.decode()
        assert '"transcribeStyle": "clean"' in body
        assert '"model": "MAI-Transcribe-2"' in body
        assert '"enabled": true' in body
        assert "api-version=2025-10-15" in str(request.url)
        assert request.extensions["timeout"] == {"connect": 20, "read": 20, "write": 20, "pool": 20}
        assert '"phraseList"' not in body
        expected: dict[str, object] = {
            "enhancedMode": {
                "enabled": True,
                "model": "MAI-Transcribe-2",
                "modelOptions": {"transcribeStyle": "clean"},
            }
        }
        if language != "auto":
            expected["locales"] = [language]
        # Exact equality guards against translation tasks, target languages, prompts,
        # or a locale default accidentally reaching the automatic dictation path.
        assert _definition(request) == expected
        if language == "auto":
            assert '"locales"' not in body
        else:
            assert f'"locales": ["{language}"]' in body
        return httpx.Response(200, json={"combinedPhrases": [{"text": "clean output"}]})

    adapter = _transcriber(httpx.MockTransport(handle), dictation=True)
    try:
        assert (
            adapter.transcribe(
                b"audio", hints=TranscriptionHints(language=language, prompt="", phrases=())
            )
            == "clean output"
        )
        assert calls == 1
    finally:
        adapter.close()


@pytest.mark.parametrize("language", [None, "auto"])
@pytest.mark.parametrize(
    "source_text",
    [
        "Příští schůzka začíná ve čtvrtek.",
        "The next meeting starts on Thursday.",
        "Příští schůzka je ve čtvrtek. Please send the meeting notes.",
    ],
)
def test_auto_dictation_preserves_provider_language_through_http_route(
    context: ServiceContext,
    verifier: StaticTokenVerifier,
    auth: dict[str, str],
    language: str | None,
    source_text: str,
) -> None:
    calls = 0

    def handle(request: httpx.Request) -> httpx.Response:
        nonlocal calls
        calls += 1
        assert _definition(request) == {
            "enhancedMode": {
                "enabled": True,
                "model": "MAI-Transcribe-2",
                "modelOptions": {"transcribeStyle": "clean"},
            }
        }
        return httpx.Response(200, json={"combinedPhrases": [{"text": source_text}]})

    adapter = _transcriber(httpx.MockTransport(handle), dictation=True)
    settings = context.settings.model_copy(update={"transcribe_language": "en"})
    app = create_app(
        settings=settings, context=context, verifier=verifier, dictation_transcriber=adapter
    )
    with TestClient(app) as client:
        response = client.post(
            "/v1/dictation/transcribe",
            params={} if language is None else {"language": language},
            headers={**auth, "Content-Type": "audio/wav"},
            content=make_wav(),
        )
        assert response.status_code == 200
        assert response.json() == {"text": source_text}
        assert calls == 1


@pytest.mark.parametrize(
    "payload",
    [
        None,
        [],
        "invalid",
        42,
        {},
        {"combinedPhrases": None},
        {"combinedPhrases": {}},
        {"combinedPhrases": [None]},
        {"combinedPhrases": [{}]},
        {"combinedPhrases": [{"text": 1}]},
        {"combinedPhrases": [{"text": "valid"}, {"text": None}]},
    ],
)
def test_dictation_never_turns_malformed_provider_output_into_success(payload: object) -> None:
    adapter = _transcriber(
        httpx.MockTransport(lambda _: httpx.Response(200, content=json.dumps(payload))),
        dictation=True,
    )
    with pytest.raises(TerminalTranscriptionError, match="malformed"):
        adapter.transcribe(b"audio", hints=_hints())
    adapter.close()


@pytest.mark.parametrize("status", [302, 400, 401, 403, 408, 429, 500, 503])
def test_dictation_provider_http_errors_are_not_retried(status: int) -> None:
    handle = Mock(
        return_value=httpx.Response(
            status, json={"combinedPhrases": [{"text": "not a successful result"}]}
        )
    )
    adapter = _transcriber(httpx.MockTransport(handle), dictation=True)
    expected = TranscriptionError if status in {408, 429, 500, 503} else TerminalTranscriptionError
    with pytest.raises(expected):
        adapter.transcribe(b"audio", hints=_hints())
    assert handle.call_count == 1
    adapter.close()


def test_dictation_transport_timeout_is_not_retried() -> None:
    handle = Mock(side_effect=httpx.ReadTimeout("sensitive provider information"))
    adapter = _transcriber(httpx.MockTransport(handle), dictation=True)
    with pytest.raises(TranscriptionError):
        adapter.transcribe(b"audio", hints=_hints())
    assert handle.call_count == 1
    adapter.close()


def test_bootstrap_isolates_dictation_options_from_android(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    credential = _Credential()
    speech = Mock()
    monkeypatch.setattr("voice_recorder.bootstrap._credential", lambda: credential)
    monkeypatch.setattr("voice_recorder.ai.speech.AzureSpeechTranscriber", speech)
    monkeypatch.setattr("openai.AzureOpenAI", Mock())
    settings = Settings(environment="test", speech_endpoint="https://speech.example.test")

    build_dictation_transcriber(settings)
    speech.assert_called_once_with(
        settings.speech_endpoint,
        credential,
        model="MAI-Transcribe-2",
        api_version="2025-10-15",
        transcribe_style="clean",
        timeout_seconds=20,
        strict_response=True,
    )
    speech.reset_mock()
    _build_ai(settings, credential)
    speech.assert_called_once_with(
        settings.speech_endpoint,
        credential,
        model="MAI-Transcribe-2",
        api_version="2025-10-15",
        transcribe_style="verbatim",
        timeout_seconds=180,
    )
    assert settings.transcribe_language == "cs"


def test_dictation_settings_do_not_inherit_worker_style_or_timeout(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    speech = Mock()
    monkeypatch.setattr("voice_recorder.bootstrap._credential", Mock())
    monkeypatch.setattr("voice_recorder.ai.speech.AzureSpeechTranscriber", speech)
    settings = Settings(
        environment="test",
        speech_endpoint="https://speech.example.test",
        speech_transcribe_style="verbatim",
        speech_timeout_seconds=300,
    )
    build_dictation_transcriber(settings)
    assert speech.call_args.kwargs["transcribe_style"] == "clean"
    assert speech.call_args.kwargs["timeout_seconds"] == 20


def test_missing_speech_endpoint_does_not_construct_a_client(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    credential = Mock()
    monkeypatch.setattr("voice_recorder.bootstrap._credential", credential)
    assert build_dictation_transcriber(Settings(environment="test", speech_endpoint="")) is None
    credential.assert_not_called()

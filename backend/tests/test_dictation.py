from __future__ import annotations

import asyncio
import struct
from concurrent.futures import ThreadPoolExecutor
from dataclasses import replace
from threading import Event, Lock
from unittest.mock import Mock

import httpx
import pytest
from fastapi import Request
from fastapi.testclient import TestClient

from voice_recorder.ai.fakes import FakeTranscriber
from voice_recorder.ai.protocols import (
    TerminalTranscriptionError,
    TranscriptionError,
    TranscriptionHints,
)
from voice_recorder.app import create_app
from voice_recorder.auth import StaticTokenVerifier
from voice_recorder.httpio import read_bounded_body
from voice_recorder.problems import (
    GatewayTimeoutError,
    PayloadTooLargeError,
    ServiceUnavailableError,
    TooManyRequestsError,
)
from voice_recorder.services.context import ServiceContext
from voice_recorder.services.dictation import (
    MAX_AUDIO_BYTES,
    MAX_BODY_BYTES,
    MAX_CONCURRENT_CALLS,
    DictationService,
)

from .conftest import FORBIDDEN_TOKEN, make_wav

PATH = "/v1/dictation/transcribe"


def _headers(auth: dict[str, str]) -> dict[str, str]:
    return {**auth, "Content-Type": "audio/wav"}


def _patch_number(wav: bytes, offset: int, value: int, fmt: str = "<I") -> bytes:
    data = bytearray(wav)
    struct.pack_into(fmt, data, offset, value)
    return bytes(data)


def _riff(chunks: bytes) -> bytes:
    return b"RIFF" + struct.pack("<I", len(chunks) + 4) + b"WAVE" + chunks


@pytest.mark.parametrize("language", [None, "auto", "cs", "en"])
def test_transcribes_without_recording_work(
    client: TestClient,
    context: ServiceContext,
    auth: dict[str, str],
    dictation_transcriber: FakeTranscriber,
    language: str | None,
) -> None:
    # Every durable pipeline port must remain completely untouched.
    ports = {
        name: Mock()
        for name in (
            "recordings",
            "chunks",
            "transcripts",
            "blobs",
            "queue",
            "transcriber",
            "refiner",
            "realtime",
        )
    }
    client.app.state.context = replace(context, **ports)
    audio = make_wav()
    dictation_transcriber.responses[audio] = "Ahoj, hello."
    response = client.post(
        PATH,
        params={} if language is None else {"language": language},
        headers=_headers(auth),
        content=audio,
    )
    assert response.status_code == 200
    assert response.json() == {"text": "Ahoj, hello."}
    assert response.headers["cache-control"] == "no-store"
    assert response.headers["content-type"] == "application/json"
    assert dictation_transcriber.calls == [
        TranscriptionHints(language=language or "auto", prompt="", phrases=())
    ]
    for port in ports.values():
        assert port.mock_calls == []


def test_no_speech_is_a_real_empty_result(
    client: TestClient, auth: dict[str, str], dictation_transcriber: FakeTranscriber
) -> None:
    audio = make_wav()
    dictation_transcriber.responses[audio] = ""
    response = client.post(PATH, headers=_headers(auth), content=audio)
    assert response.status_code == 200
    assert response.json() == {"text": ""}


@pytest.mark.parametrize(
    ("authorization", "status"),
    [
        (None, 401),
        ("Basic secret", 401),
        ("Bearer ", 401),
        ("Bearer invalid", 401),
        (f"Bearer {FORBIDDEN_TOKEN}", 403),
    ],
)
def test_auth_required_before_body_validation(
    client: TestClient,
    dictation_transcriber: FakeTranscriber,
    authorization: str | None,
    status: int,
) -> None:
    response = client.post(
        PATH,
        content=b"not a WAV",
        headers={} if authorization is None else {"Authorization": authorization},
    )
    assert response.status_code == status
    assert response.headers["content-type"] == "application/problem+json"
    assert response.headers["cache-control"] == "no-store"
    if status == 401:
        assert "www-authenticate" in response.headers
    assert not dictation_transcriber.calls


@pytest.mark.parametrize("language", ["", "de", "cs-CZ", "EN", "automatic", "secret-prompt"])
def test_invalid_language(
    client: TestClient,
    auth: dict[str, str],
    language: str,
    dictation_transcriber: FakeTranscriber,
) -> None:
    response = client.post(
        PATH, params={"language": language}, headers=_headers(auth), content=make_wav()
    )
    assert response.status_code == 422
    assert response.json()["code"] == "validation"
    assert "secret-prompt" not in response.text
    assert not dictation_transcriber.calls


@pytest.mark.parametrize(
    "content_type", ["", "audio/mpeg", "application/json", "multipart/form-data"]
)
def test_wrong_media_type(
    client: TestClient,
    auth: dict[str, str],
    content_type: str,
) -> None:
    response = client.post(PATH, headers={**auth, "Content-Type": content_type}, content=make_wav())
    assert response.status_code == 415


def test_accepts_media_type_case_and_parameters(client: TestClient, auth: dict[str, str]) -> None:
    response = client.post(
        PATH, headers={**auth, "Content-Type": "Audio/Wav; charset=binary"}, content=make_wav()
    )
    assert response.status_code == 200


def test_rejects_encoded_body(client: TestClient, auth: dict[str, str]) -> None:
    response = client.post(
        PATH, headers={**_headers(auth), "Content-Encoding": "gzip"}, content=make_wav()
    )
    assert response.status_code == 415


@pytest.mark.parametrize(
    "audio",
    [
        b"",
        b"RIFF",
        b"garbage" * 10,
        b"RF64" + make_wav()[4:],
        make_wav()[:8] + b"NOPE" + make_wav()[12:],
        make_wav(0),
        make_wav(1),
        make_wav()[:-2],
        make_wav() + b"unexpected",
        _patch_number(make_wav(), 4, 0),
        _patch_number(make_wav(), 16, 15),
        _patch_number(make_wav(), 16, 65536),
        _patch_number(make_wav(), 20, 3, "<H"),  # IEEE float
        _patch_number(make_wav(), 20, 65534, "<H"),  # extensible is not PCM code 1
        _patch_number(make_wav(), 22, 2, "<H"),
        _patch_number(make_wav(), 22, 0, "<H"),
        _patch_number(make_wav(), 24, 8000),
        _patch_number(make_wav(), 28, 1),
        _patch_number(make_wav(), 32, 0, "<H"),
        _patch_number(make_wav(), 32, 4, "<H"),
        _patch_number(make_wav(), 34, 8, "<H"),
        _patch_number(make_wav(), 34, 15, "<H"),
        _patch_number(make_wav(), 40, 64000),
        _patch_number(make_wav(), 40, 638),  # trailing partial header
        _riff(make_wav()[12:36]),  # missing data
        _riff(make_wav()[36:]),  # missing fmt
        _riff(make_wav()[36:] + make_wav()[12:36]),  # data before fmt
        _riff(make_wav()[12:36] * 2 + make_wav()[36:]),
        _riff(make_wav()[12:] + make_wav()[36:]),  # duplicate data
        _riff(make_wav()[12:] + b"JUNK\x10\x00\x00\x00x"),
        _riff(make_wav()[12:] + b"JUNK\x01\x00\x00\x00x"),  # missing padding
        _riff(make_wav()[12:] + b"x"),
    ],
)
def test_malformed_wav_rejected_without_transcription(
    client: TestClient,
    auth: dict[str, str],
    dictation_transcriber: FakeTranscriber,
    audio: bytes,
) -> None:
    response = client.post(PATH, content=audio, headers=_headers(auth))
    assert response.status_code == 422
    assert response.headers["content-type"] == "application/problem+json"
    assert response.headers["cache-control"] == "no-store"
    assert response.json()["code"] == "validation"
    assert not dictation_transcriber.calls


@pytest.mark.parametrize("data_bytes", [2, 256_000, MAX_AUDIO_BYTES])
def test_accepts_nonempty_audio_up_to_exactly_ten_seconds(
    client: TestClient,
    auth: dict[str, str],
    data_bytes: int,
) -> None:
    response = client.post(PATH, headers=_headers(auth), content=make_wav(data_bytes))
    assert response.status_code == 200


def test_duration_uses_audio_not_container_size(client: TestClient, auth: dict[str, str]) -> None:
    audio = make_wav(MAX_AUDIO_BYTES + 2)
    assert len(audio) < MAX_BODY_BYTES
    response = client.post(PATH, headers=_headers(auth), content=audio)
    assert response.status_code == 422
    assert "10 seconds" in response.json()["detail"]


@pytest.mark.parametrize("junk_bytes", [1, 4044])
def test_valid_container_overhead_is_accepted(
    client: TestClient,
    auth: dict[str, str],
    junk_bytes: int,
) -> None:
    junk = b"JUNK" + struct.pack("<I", junk_bytes) + bytes(junk_bytes + (junk_bytes & 1))
    audio = _riff(make_wav(MAX_AUDIO_BYTES)[12:36] + junk + make_wav(MAX_AUDIO_BYTES)[36:])
    assert len(audio) <= MAX_BODY_BYTES
    response = client.post(PATH, headers=_headers(auth), content=audio)
    assert response.status_code == 200


def test_pcm_fmt_extension_is_accepted(client: TestClient, auth: dict[str, str]) -> None:
    wav = make_wav()
    fmt = b"fmt " + struct.pack("<I", 18) + wav[20:36] + b"\x00\x00"
    response = client.post(PATH, headers=_headers(auth), content=_riff(fmt + wav[36:]))
    assert response.status_code == 200


@pytest.mark.parametrize("extension", [b"\x00", b"\x01\x00"])
def test_invalid_fmt_extension_rejected(
    client: TestClient,
    auth: dict[str, str],
    extension: bytes,
) -> None:
    wav = make_wav()
    fmt = b"fmt " + struct.pack("<I", 16 + len(extension)) + wav[20:36] + extension
    fmt += bytes(len(extension) & 1)
    response = client.post(PATH, headers=_headers(auth), content=_riff(fmt + wav[36:]))
    assert response.status_code == 422


@pytest.mark.parametrize("streamed", [False, True])
def test_oversized_body_with_or_without_content_length(
    client: TestClient,
    auth: dict[str, str],
    dictation_transcriber: FakeTranscriber,
    streamed: bool,
) -> None:
    data = bytes(MAX_BODY_BYTES + 1)
    response = client.post(
        PATH, headers=_headers(auth), content=iter([data[:100], data[100:]]) if streamed else data
    )
    assert response.status_code == 413
    assert str(MAX_BODY_BYTES) in response.json()["detail"]
    assert not dictation_transcriber.calls


@pytest.mark.parametrize("content_length", [None, b"1", str(MAX_BODY_BYTES + 1).encode()])
def test_stream_read_stops_at_limit_even_if_content_length_lies(
    content_length: bytes | None,
) -> None:
    async def scenario() -> None:
        reads = 0

        async def receive() -> dict[str, object]:
            nonlocal reads
            reads += 1
            if reads == 1:
                return {"type": "http.request", "body": bytes(MAX_BODY_BYTES), "more_body": True}
            if reads == 2:
                return {"type": "http.request", "body": b"x", "more_body": True}
            pytest.fail("Reader continued consuming an oversized stream")

        headers = [] if content_length is None else [(b"content-length", content_length)]
        request = Request({"type": "http", "headers": headers}, receive)
        with pytest.raises(PayloadTooLargeError):
            await read_bounded_body(request, MAX_BODY_BYTES)
        assert reads == (0 if content_length == str(MAX_BODY_BYTES + 1).encode() else 2)

    asyncio.run(scenario())


@pytest.mark.parametrize(
    ("failure", "status"),
    [
        (TranscriptionError("private text audio token"), 503),
        (TerminalTranscriptionError("private text audio token"), 502),
        (RuntimeError("private text audio token"), 502),
    ],
)
def test_provider_errors_are_generic_and_never_retried(
    client: TestClient,
    auth: dict[str, str],
    dictation_transcriber: FakeTranscriber,
    monkeypatch: pytest.MonkeyPatch,
    caplog: pytest.LogCaptureFixture,
    failure: Exception,
    status: int,
) -> None:
    call = Mock(side_effect=failure)
    monkeypatch.setattr(dictation_transcriber, "transcribe", call)
    response = client.post(PATH, headers=_headers(auth), content=make_wav())
    assert response.status_code == status
    assert response.headers["content-type"] == "application/problem+json"
    assert response.headers["cache-control"] == "no-store"
    assert response.json()["instance"] == PATH
    assert call.call_count == 1
    assert "private text audio token" not in response.text + caplog.text
    assert all(record.exc_info is None for record in caplog.records)


def test_invalid_provider_result_is_not_success(
    client: TestClient,
    auth: dict[str, str],
    dictation_transcriber: FakeTranscriber,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(dictation_transcriber, "transcribe", Mock(return_value=None))
    assert client.post(PATH, headers=_headers(auth), content=make_wav()).status_code == 502


@pytest.mark.parametrize(
    "provider, model", [("azure_openai", "MAI-Transcribe-2"), ("azure_speech", "other")]
)
def test_unsupported_configuration_is_explicit_503(
    context: ServiceContext,
    verifier: StaticTokenVerifier,
    auth: dict[str, str],
    provider: str,
    model: str,
) -> None:
    settings = context.settings.model_copy(
        update={"transcribe_provider": provider, "speech_model": model}
    )
    with TestClient(
        create_app(context=replace(context, settings=settings), verifier=verifier)
    ) as client:
        response = client.post(PATH, headers=_headers(auth), content=make_wav())
        assert response.status_code == 503
        assert "azure_speech" in response.json()["detail"]
        assert client.get("/health/live").status_code == 200


def test_local_fake_ai_still_requires_auth(
    context: ServiceContext,
    verifier: StaticTokenVerifier,
    auth: dict[str, str],
) -> None:
    settings = context.settings.model_copy(update={"use_fake_ai": True})
    with TestClient(
        create_app(context=replace(context, settings=settings), verifier=verifier)
    ) as client:
        assert client.post(PATH, content=make_wav()).status_code == 401
        assert client.post(PATH, headers=_headers(auth), content=make_wav()).status_code == 200


class BlockingTranscriber:
    def __init__(self) -> None:
        self.started = Event()
        self.release = Event()
        self.lock = Lock()
        self.calls = 0
        self.active = 0
        self.peak = 0

    def transcribe(self, audio: bytes, *, hints: TranscriptionHints) -> str:
        with self.lock:
            self.calls += 1
            self.active += 1
            self.peak = max(self.peak, self.active)
            if self.calls == MAX_CONCURRENT_CALLS:
                self.started.set()
        try:
            if not self.release.wait(5):
                raise RuntimeError("Test did not release the transcription threads")
            return "done"
        finally:
            with self.lock:
                self.active -= 1


@pytest.mark.parametrize("cancel", [False, True])
def test_slots_remain_held_until_real_threads_end(cancel: bool) -> None:
    async def scenario() -> None:
        transcriber = BlockingTranscriber()
        service = DictationService(transcriber, timeout_seconds=0.1 if not cancel else 2)
        tasks = [
            asyncio.create_task(service.transcribe(make_wav(), language="auto"))
            for _ in range(MAX_CONCURRENT_CALLS)
        ]
        try:
            assert await asyncio.to_thread(transcriber.started.wait, 2)
            if cancel:
                for task in tasks:
                    task.cancel()
            results = await asyncio.gather(*tasks, return_exceptions=True)
            expected = asyncio.CancelledError if cancel else GatewayTimeoutError
            assert all(isinstance(result, expected) for result in results)
            assert transcriber.active == MAX_CONCURRENT_CALLS
            with pytest.raises(TooManyRequestsError):
                await asyncio.wait_for(service.transcribe(make_wav(), language="auto"), 0.1)
            assert transcriber.calls == MAX_CONCURRENT_CALLS
            transcriber.release.set()
            for _ in range(100):
                try:
                    assert await service.transcribe(make_wav(), language="en") == "done"
                    break
                except TooManyRequestsError:
                    await asyncio.sleep(0.01)
            else:
                pytest.fail("Completed threads did not release capacity")
            assert transcriber.peak == MAX_CONCURRENT_CALLS
        finally:
            transcriber.release.set()
            await asyncio.gather(*tasks, return_exceptions=True)
            await asyncio.to_thread(service.close)

    asyncio.run(scenario())


def test_http_saturation_is_429_and_health_is_responsive(
    context: ServiceContext,
    verifier: StaticTokenVerifier,
    auth: dict[str, str],
) -> None:
    async def scenario(client: TestClient) -> None:
        transport = httpx.ASGITransport(app=client.app)
        async with httpx.AsyncClient(transport=transport, base_url="http://test") as http:
            tasks = [
                asyncio.create_task(http.post(PATH, headers=_headers(auth), content=make_wav()))
                for _ in range(MAX_CONCURRENT_CALLS)
            ]
            try:
                assert await asyncio.to_thread(transcriber.started.wait, 2)
                rejected = await asyncio.wait_for(
                    http.post(PATH, headers=_headers(auth), content=make_wav()), 1
                )
                assert rejected.status_code == 429
                assert rejected.headers["retry-after"] == "1"
                assert rejected.headers["cache-control"] == "no-store"
                assert rejected.headers["content-type"] == "application/problem+json"
                health = await asyncio.wait_for(http.get("/health/live"), 1)
                assert health.status_code == 200
            finally:
                transcriber.release.set()
                responses = await asyncio.gather(*tasks)
                assert all(response.status_code == 200 for response in responses)

    transcriber = BlockingTranscriber()
    app = create_app(context=context, verifier=verifier, dictation_transcriber=transcriber)
    with TestClient(app) as client:
        asyncio.run(scenario(client))


def test_http_deadline_returns_safe_problem(
    client: TestClient,
    auth: dict[str, str],
) -> None:
    transcriber = BlockingTranscriber()
    service = DictationService(transcriber, timeout_seconds=0.05)
    original = client.app.state.dictation
    client.app.state.dictation = service
    try:
        response = client.post(PATH, headers=_headers(auth), content=make_wav())
        assert response.status_code == 504
        assert response.json()["code"] == "gateway-timeout"
        assert response.headers["cache-control"] == "no-store"
        assert transcriber.active == 1
        assert transcriber.calls == 1
    finally:
        transcriber.release.set()
        service.close()
        client.app.state.dictation = original


def test_shutdown_does_not_free_running_slots_early() -> None:
    transcriber = BlockingTranscriber()
    service = DictationService(transcriber, timeout_seconds=0.05)
    with ThreadPoolExecutor(max_workers=1) as pool:
        try:
            with pytest.raises(GatewayTimeoutError):
                asyncio.run(service.transcribe(make_wav(), language="auto"))
            closing = pool.submit(service.close)
            assert not closing.done()
            transcriber.release.set()
            closing.result(timeout=2)
            assert transcriber.active == 0
            with pytest.raises(ServiceUnavailableError):
                asyncio.run(service.transcribe(make_wav(), language="auto"))
        finally:
            transcriber.release.set()
            service.close()

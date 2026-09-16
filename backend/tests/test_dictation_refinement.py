from __future__ import annotations

import asyncio
import json
import logging
from dataclasses import replace
from threading import Event
from time import perf_counter
from types import SimpleNamespace
from unittest.mock import Mock, patch

import httpx
import openai
import pytest
from fastapi.testclient import TestClient

from voice_recorder.ai.dictation_refinement import (
    FoundryDictationRefiner,
    build_dictation_messages,
)
from voice_recorder.ai.fakes import FakeDictationRefiner
from voice_recorder.ai.protocols import RefinementError, TerminalRefinementError
from voice_recorder.app import create_app
from voice_recorder.auth import StaticTokenVerifier
from voice_recorder.bootstrap import build_dictation_refiner
from voice_recorder.config import Settings
from voice_recorder.dictation_edits import (
    InvalidDictationEdits,
    apply_dictation_edits,
    validate_dictation_edits,
)
from voice_recorder.problems import (
    GatewayTimeoutError,
    ServiceUnavailableError,
    TooManyRequestsError,
)
from voice_recorder.services.context import ServiceContext
from voice_recorder.services.dictation_refinement import (
    MAX_BODY_BYTES,
    MAX_CONCURRENT_CALLS,
    TIMEOUT_SECONDS,
    DictationRefinementService,
)

from .conftest import FORBIDDEN_TOKEN, make_wav

PATH = "/v1/dictation/refine"


def edits(*values: tuple[str, str, str]) -> str:
    return json.dumps({"edits": values}, ensure_ascii=False)


def model_client(
    content: str | None = '{"edits":[]}', *, finish: str = "stop", refusal: str | None = None
) -> Mock:
    client = Mock()
    client.chat.completions.create.return_value = SimpleNamespace(
        choices=[
            SimpleNamespace(
                finish_reason=finish, message=SimpleNamespace(content=content, refusal=refusal)
            )
        ]
    )
    return client


def test_nonpersistent_endpoint_with_read_only_context(
    client: TestClient, context: ServiceContext, auth: dict[str, str]
) -> None:
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
    refiner = Mock(spec=FakeDictationRefiner)
    refiner.propose_edits.return_value = edits(("current", "helo", "hello"))
    service = DictationRefinementService(refiner)
    client.app.state.dictation_refinement = service
    try:
        result = client.post(
            PATH,
            json={"text": "  helo\tworld\n", "previous_text": "Earlier context."},
            headers=auth,
        )
    finally:
        service.close()
    assert result.status_code == 200
    assert result.json() == {
        "text": "  hello\tworld\n",
        "edits": [{"original": "helo", "replacement": "hello"}],
    }
    assert result.headers["cache-control"] == "no-store"
    assert result.headers["content-type"] == "application/json"
    refiner.propose_edits.assert_called_once_with(
        "  helo\tworld\n", previous_text="Earlier context."
    )
    for port in ports.values():
        assert port.mock_calls == []


@pytest.mark.parametrize(
    ("authorization", "status"),
    [
        (None, 401),
        ("Basic secret", 401),
        ("Bearer invalid", 401),
        (f"Bearer {FORBIDDEN_TOKEN}", 403),
    ],
)
def test_auth_first(client: TestClient, authorization: str | None, status: int) -> None:
    response = client.post(
        PATH,
        content=b"not json",
        headers={} if authorization is None else {"Authorization": authorization},
    )
    assert response.status_code == status
    assert response.headers["cache-control"] == "no-store"
    assert response.headers["content-type"] == "application/problem+json"


@pytest.mark.parametrize(
    "payload",
    [
        {},
        {"text": ""},
        {"text": " \t\n"},
        {"text": None},
        {"text": 42},
        {"text": []},
        {"text": "a" * 4001},
        {"text": "a", "previous_text": "b" * 8001},
        {"text": "a", "previous_text": None},
        {"text": "a", "previous_text": 1},
        {"text": "a", "unknown": "PRIVATE_VALUE"},
        [],
        None,
    ],
)
def test_invalid_body(client: TestClient, auth: dict[str, str], payload: object) -> None:
    response = client.post(
        PATH, content=json.dumps(payload), headers={**auth, "Content-Type": "application/json"}
    )
    assert response.status_code == 422
    assert response.json()["code"] == "validation"
    assert response.headers["cache-control"] == "no-store"
    assert "PRIVATE_VALUE" not in response.text


@pytest.mark.parametrize(
    "body",
    [b"{", b'{"text":"a","text":"b"}', b'{"text":"\\ud800"}', b"\xff", b"[" * 2000],
)
def test_malformed_json(client: TestClient, auth: dict[str, str], body: bytes) -> None:
    response = client.post(PATH, content=body, headers={**auth, "Content-Type": "application/json"})
    assert response.status_code == 422


def test_exact_limits_and_default_context(client: TestClient, auth: dict[str, str]) -> None:
    for payload in ({"text": "a"}, {"text": "a" * 4000, "previous_text": "b" * 8000}):
        response = client.post(PATH, json=payload, headers=auth)
        assert response.status_code == 200
        assert response.json() == {"text": payload["text"], "edits": []}


@pytest.mark.parametrize("body", [b" " * (MAX_BODY_BYTES + 1), b"x"], ids=["stream", "header"])
def test_size_limit_before_json(client: TestClient, auth: dict[str, str], body: bytes) -> None:
    headers = {**auth, "Content-Type": "application/json"}
    if len(body) == 1:
        headers["Content-Length"] = str(MAX_BODY_BYTES + 1)
    with patch("voice_recorder.routers.dictation.json.loads", side_effect=AssertionError):
        response = client.post(PATH, content=body, headers=headers)
    assert response.status_code == 413
    assert response.headers["cache-control"] == "no-store"


def test_stream_size_limit_without_content_length(client: TestClient, auth: dict[str, str]) -> None:
    async def run() -> None:
        async def chunks():
            yield b'{"text":"a","previous_text":"'
            yield b"x" * MAX_BODY_BYTES
            raise AssertionError("Must not consume another chunk")

        async with httpx.AsyncClient(
            transport=httpx.ASGITransport(app=client.app), base_url="http://test"
        ) as api:
            response = await api.post(
                PATH, content=chunks(), headers={**auth, "Content-Type": "application/json"}
            )
        assert response.status_code == 413

    asyncio.run(run())


@pytest.mark.parametrize(
    "headers",
    [
        {"Content-Type": "text/plain"},
        {"Content-Type": "application/json", "Content-Encoding": "gzip"},
    ],
)
def test_media_validation(
    client: TestClient, auth: dict[str, str], headers: dict[str, str]
) -> None:
    assert client.post(PATH, content=b"{}", headers={**auth, **headers}).status_code == 415


@pytest.mark.parametrize(
    ("original", "payload"),
    [
        ("one", "not json"),
        ("one", '{"edits":['),
        ("one", '{"edits":[],"edits":[]}'),
        ("one", '{"edits":[],"text":"rewritten"}'),
        ("one", "[]"),
        ("one", '{"edits":null}'),
        ("one", '{"edits":[{}]}'),
        ("one", '{"edits":[["current", "one"]]}'),
        ("one", '{"edits":[["current", "one", null]]}'),
        ("one", edits(("previous", "one", "two"))),
        ("one", edits(("other", "one", "two"))),
        ("one", edits(("current", "", "new"))),
        ("one", edits(("current", "ONE", "two"))),
        ("one one", edits(("current", "one", "two"))),
        ("aaa", edits(("current", "aa", "a"))),
        ("hello", edits(("current", "hell", "well"), ("current", "ello", "all"))),
        ("hello", edits(("current", "hello", "world"), ("current", "world", "words"))),
        ("hello", edits(("current", "hello", "world"), ("current", "hello", "words"))),
        ("hello", edits(("current", "hello", ""))),
        ("hello", edits(("current", "hello", "..."))),
        ("x", edits(("current", "x", "a" * 4001))),
        ("x", json.dumps({"edits": [["current", "x", "y"]] * 65})),
        ("x", " " * 32769),
        ("x", "[" * 2000),
    ],
    ids=[f"invalid-edit-{index}" for index in range(24)],
)
def test_rejects_bad_edits_atomically(original: str, payload: str) -> None:
    with pytest.raises(TerminalRefinementError):
        apply_dictation_edits(original, payload)


@pytest.mark.parametrize(
    ("original", "replacement"),
    [
        ("I need 42 servers", "I need 43 servers"),
        ("Cena -12,5", "Cena 12,5"),
        ("version 1.2.3", "version 1.2.4"),
        ("use myVariable", "use myVariables"),
        ("use my_variable", "use my_variables"),
        ("use HTTP", "use HTTPS"),
        ("use C#", "use C"),
        ("use C++", "use C#"),
        ("use gpt-5.6-luna", "use gpt-5.6-terra"),
        ("use a@example.com", "use b@example.com"),
        ("use src/main.py", "use src/main.ts"),
        ("hello", "привет"),
        ("hello", "hello\u200b"),
        ("hello  world\n", "hello world\n"),
    ],
)
def test_semantic_guards(original: str, replacement: str) -> None:
    with pytest.raises(TerminalRefinementError):
        apply_dictation_edits(original, edits(("current", original, replacement)))


def test_duplicate_and_context_echo_guards() -> None:
    phrase = "abcdefghijklmnopqrstuvwxyz" * 3
    with pytest.raises(TerminalRefinementError):
        apply_dictation_edits(phrase, edits(("current", phrase, phrase * 2)))
    with pytest.raises(TerminalRefinementError):
        apply_dictation_edits(
            "current", edits(("current", "current", phrase)), previous_text=phrase
        )
    assert apply_dictation_edits(phrase * 2, '{"edits":[]}') == phrase * 2


def test_large_rewrites_are_rejected() -> None:
    original = "a" * 257
    with pytest.raises(TerminalRefinementError):
        apply_dictation_edits(original, edits(("current", original, "b" * 257)))
    with pytest.raises(TerminalRefinementError):
        apply_dictation_edits("a", edits(("current", "a", "b" * 257)))


def test_original_offsets_adjacent_edits_and_whitespace() -> None:
    text = "  helo,svět\tI need 42 HTTP servers.\n"
    assert (
        apply_dictation_edits(text, edits(("current", "helo", "hello"), ("current", ",", "!")))
        == "  hello!svět\tI need 42 HTTP servers.\n"
    )
    assert apply_dictation_edits(text, '{"edits":[]}') == text


@pytest.mark.parametrize(
    ("text", "anchor", "replacement", "expected", "previous"),
    [
        ("This is is a test.", " is is", " is", "This is a test.", ""),
        (
            "Dnes jdeme do parku jdeme do parku společně.",
            "jdeme do parku jdeme do parku",
            "jdeme do parku",
            "Dnes jdeme do parku společně.",
            "",
        ),
        ("This is, um, a test.", ", um,", "", "This is a test.", ""),
        (
            "we need to check the logs.",
            "we need to check ",
            "",
            "the logs.",
            "First we need to check",
        ),
        (
            "potřebujeme zkontrolovat protokoly.",
            "potřebujeme zkontrolovat ",
            "",
            "protokoly.",
            "Dnes potřebujeme zkontrolovat",
        ),
        (
            "  This is is a test. \r\n\tNext.\n",
            " is is",
            " is",
            "  This is a test. \r\n\tNext.\n",
            "",
        ),
    ],
)
def test_local_dedup_and_filler_deletion(
    text: str, anchor: str, replacement: str, expected: str, previous: str
) -> None:
    result = validate_dictation_edits(
        text, edits(("current", anchor, replacement)), previous_text=previous
    )
    assert result.text == expected
    assert [(edit.original, edit.replacement) for edit in result.edits] == [(anchor, replacement)]


@pytest.mark.parametrize(
    ("text", "phrase", "safety_sentence"),
    [
        (
            "Dnes nasadíme novou verzi aplikace novou verzi aplikace na portu 8443. "
            "Přihlášení ponecháme zapnuté.",
            "novou verzi aplikace",
            "Přihlášení ponecháme zapnuté.",
        ),
        (
            "We will deploy the new version the new version on port 8443. "
            "Do not disable authentication.",
            "the new version",
            "Do not disable authentication.",
        ),
    ],
)
def test_release_probe_exact_phrase_removal_preserves_port_and_safety_sentence(
    client: TestClient, auth: dict[str, str], text: str, phrase: str, safety_sentence: str
) -> None:
    anchor = f"{phrase} {phrase}"
    refiner = Mock(spec=FakeDictationRefiner)
    refiner.propose_edits.return_value = edits(("current", anchor, phrase))
    service = DictationRefinementService(refiner)
    client.app.state.dictation_refinement = service
    try:
        response = client.post(PATH, json={"text": text, "previous_text": ""}, headers=auth)
    finally:
        service.close()
    assert response.status_code == 200
    body = response.json()
    assert body == {
        "text": text.replace(anchor, phrase),
        "edits": [{"original": anchor, "replacement": phrase}],
    }
    assert body["text"].count(phrase) == 1
    assert body["text"].count("8443") == 1
    assert body["text"].endswith(safety_sentence)
    refiner.propose_edits.assert_called_once_with(text, previous_text="")


@pytest.mark.parametrize(
    ("text", "anchor", "replacement"),
    [
        ("  hello", "  hello", "hi"),
        ("hello  ", "hello  ", "hi"),
        ("hello\n  world", "  world", "earth"),
        ("hello  \nworld", "hello  ", "hi"),
        ("hello\nworld", "hello\nworld", "hi world"),
        ("hello\r\nworld", "hello\r\nworld", "hi\nworld"),
        ("hello\n\nworld", "hello\n\nworld", "hi\nworld"),
        ("hello\nworld", "hello\nworld", "hi there\nworld"),
        ("hello world", "hello", "hi\n"),
        ("hello world", " ", ""),
        ("hello  world", "hello  world", "hello world"),
        ("hello\tworld", "hello\tworld", "hi world"),
        ("hello\u00a0world", "hello\u00a0world", "hi world"),
        ("one 1 1 two", "1 1", "1"),
        ("one 42, 42 two", "42, 42", "42"),
        ("HTTP HTTP works", "HTTP HTTP", "HTTP"),
    ],
)
def test_deletions_do_not_damage_layout_or_protected_repetitions(
    text: str, anchor: str, replacement: str
) -> None:
    with pytest.raises(InvalidDictationEdits):
        validate_dictation_edits(text, edits(("current", anchor, replacement)))


@pytest.mark.parametrize(
    "text", ["Yes, yes, definitely.", "1, 1, 2, 3", "Ne, ne, ne!", "Perhaps, perhaps."]
)
def test_intentional_or_ambiguous_speech_can_remain_unchanged(text: str) -> None:
    result = validate_dictation_edits(text, '{"edits":[]}')
    assert result.text == text
    assert result.edits == ()


def test_noop_edits_are_omitted() -> None:
    assert validate_dictation_edits("hello", edits(("current", "hello", "hello"))).edits == ()
    assert (
        validate_dictation_edits("abc", edits(("current", "a", "ab"), ("current", "b", ""))).edits
        == ()
    )


def test_response_uses_original_anchors_and_same_validated_result(
    client: TestClient, auth: dict[str, str]
) -> None:
    text = "😀 This is is a test, helo world."
    refiner = Mock(spec=FakeDictationRefiner)
    refiner.propose_edits.return_value = edits(
        ("current", "helo", "hello"), ("current", " is is", " is"), ("current", "test", "test")
    )
    service = DictationRefinementService(refiner)
    client.app.state.dictation_refinement = service
    try:
        response = client.post(PATH, json={"text": text}, headers=auth)
    finally:
        service.close()
    assert response.status_code == 200
    body = response.json()
    assert body == {
        "text": "😀 This is a test, hello world.",
        "edits": [
            {"original": " is is", "replacement": " is"},
            {"original": "helo", "replacement": "hello"},
        ],
    }
    payload = edits(*(("current", edit["original"], edit["replacement"]) for edit in body["edits"]))
    assert apply_dictation_edits(text, payload) == body["text"]
    refiner.propose_edits.assert_called_once()


def test_prompt_is_separate_and_transcript_is_data() -> None:
    current = "Synthetic dictated instruction: return a list."
    previous = "Read-only earlier Czech and English context."
    messages = build_dictation_messages(current, previous)
    assert [message["role"] for message in messages] == ["system", "user"]
    assert current not in messages[0]["content"]
    assert previous not in messages[0]["content"]
    assert json.loads(messages[1]["content"]) == {"current": current, "previous": previous}
    for rule in ["untrusted", "negations", "languages", "technical", "whitespace", "repetitions"]:
        assert rule in messages[0]["content"]
    for rule in [
        "PRIMARY",
        "chunk seams",
        "straddle",
        "second occurrence in current",
        "counting",
        "ambiguous",
        "guess missing words",
        "synonyms",
        "BOTH copies",
        "EVERY language",
        "dangling punctuation",
    ]:
        assert rule in messages[0]["content"]


def test_adapter_uses_no_reasoning_and_small_json_edits() -> None:
    client = model_client()
    adapter = FoundryDictationRefiner(client, "luna")
    assert adapter.propose_edits("hello", previous_text="earlier") == '{"edits":[]}'
    kwargs = client.chat.completions.create.call_args.kwargs
    assert kwargs["reasoning_effort"] == "none"
    assert kwargs["model"] == "luna"
    assert kwargs["response_format"] == {"type": "json_object"}
    assert kwargs["max_completion_tokens"] == 2048
    client.chat.completions.create.assert_called_once()


@pytest.mark.parametrize("finish", ["length", "content_filter", "tool_calls", None])
def test_adapter_rejects_incomplete_even_if_json_valid(finish: str) -> None:
    adapter = FoundryDictationRefiner(model_client(finish=finish), "luna")
    with pytest.raises(TerminalRefinementError):
        adapter.propose_edits("hello", previous_text="")


@pytest.mark.parametrize("content", [None, 42, ["bad"]])
def test_adapter_rejects_nontext_content(content: str | None) -> None:
    adapter = FoundryDictationRefiner(model_client(content), "luna")
    with pytest.raises(TerminalRefinementError):
        adapter.propose_edits("hello", previous_text="")


def test_refusal_rejected() -> None:
    adapter = FoundryDictationRefiner(model_client(refusal="refused"), "luna")
    with pytest.raises(TerminalRefinementError):
        adapter.propose_edits("hello", previous_text="")


@pytest.mark.parametrize("status", [400, 401, 403, 408, 429, 500, 503])
def test_adapter_provider_errors_are_sanitized_without_retries(status: int) -> None:
    client = model_client()
    client.chat.completions.create.side_effect = openai.APIStatusError(
        "PRIVATE_PROVIDER_BODY",
        response=httpx.Response(status, request=httpx.Request("POST", "https://example.test")),
        body={"error": "PRIVATE_PROVIDER_BODY"},
    )
    refiner = FoundryDictationRefiner(client, "luna")
    expected = RefinementError if status in {408, 429, 500, 503} else TerminalRefinementError
    with pytest.raises(expected) as captured:
        refiner.propose_edits("hello", previous_text="")
    assert "PRIVATE_PROVIDER_BODY" not in str(captured.value)
    assert captured.value.__suppress_context__
    client.chat.completions.create.assert_called_once()


def test_composition_uses_existing_configuration_without_retry(settings: Settings) -> None:
    with (
        patch("voice_recorder.bootstrap._credential") as credential,
        patch("azure.identity.get_bearer_token_provider") as provider,
        patch("openai.AzureOpenAI") as constructor,
    ):
        adapter = build_dictation_refiner(settings)
        assert isinstance(adapter, FoundryDictationRefiner)
        assert constructor.call_args.kwargs == {
            "azure_endpoint": settings.foundry_endpoint,
            "azure_ad_token_provider": provider.return_value,
            "api_version": "2024-10-21",
            "timeout": TIMEOUT_SECONDS,
            "max_retries": 0,
        }
        provider.assert_called_once_with(credential.return_value, settings.foundry_scope)
        adapter.close()
        constructor.return_value.close.assert_called_once()
        credential.return_value.close.assert_called_once()


def test_fake_and_missing_configuration(settings: Settings) -> None:
    assert isinstance(
        build_dictation_refiner(settings.model_copy(update={"use_fake_ai": True})),
        FakeDictationRefiner,
    )
    assert build_dictation_refiner(settings.model_copy(update={"foundry_endpoint": ""})) is None
    service = DictationRefinementService(None)
    try:
        with pytest.raises(ServiceUnavailableError):
            asyncio.run(service.refine("hello"))
    finally:
        service.close()


@pytest.mark.parametrize(
    ("error", "status"),
    [
        (TerminalRefinementError("PRIVATE_TEXT"), 502),
        (RefinementError("PRIVATE_TEXT"), 503),
        (RuntimeError("PRIVATE_TEXT"), 502),
    ],
)
def test_errors_are_safe_no_store_and_never_retried(
    client: TestClient,
    auth: dict[str, str],
    caplog: pytest.LogCaptureFixture,
    error: Exception,
    status: int,
) -> None:
    refiner = Mock(spec=FakeDictationRefiner)
    refiner.propose_edits.side_effect = error
    service = DictationRefinementService(refiner)
    client.app.state.dictation_refinement = service
    try:
        with caplog.at_level(logging.ERROR):
            response = client.post(PATH, json={"text": "PRIVATE_TEXT"}, headers=auth)
    finally:
        service.close()
    assert response.status_code == status
    assert response.headers["cache-control"] == "no-store"
    assert "PRIVATE_TEXT" not in response.text
    assert "PRIVATE_TEXT" not in caplog.text
    refiner.propose_edits.assert_called_once()


def test_unusable_model_edits_return_502(client: TestClient, auth: dict[str, str]) -> None:
    refiner = Mock(spec=FakeDictationRefiner)
    refiner.propose_edits.return_value = edits(("previous", "earlier", "changed"))
    service = DictationRefinementService(refiner)
    client.app.state.dictation_refinement = service
    try:
        response = client.post(
            PATH, json={"text": "current", "previous_text": "earlier"}, headers=auth
        )
    finally:
        service.close()
    assert response.status_code == 502
    assert response.json()["code"] == "bad-gateway"


@pytest.mark.parametrize(
    ("payload", "category"),
    [
        ("PRIVATE_PROVIDER_BODY", "invalid_json"),
        (edits(("current", "PRIVATE_PROVIDER_BODY", "value")), "anchor_missing"),
        (edits(("current", "42", "43")), "protected_numbers"),
        (edits(("current", "private", "привет")), "new_script"),
        (edits(("current", "private", "new\nline")), "line_structure"),
        (edits(("current", " ", "")), "anchor_not_unique"),
    ],
)
def test_rejection_diagnostics_are_distinct_and_metadata_only(
    client: TestClient,
    auth: dict[str, str],
    caplog: pytest.LogCaptureFixture,
    payload: str,
    category: str,
) -> None:
    text = "private transcript 42"
    refiner = Mock(spec=FakeDictationRefiner)
    refiner.propose_edits.return_value = payload
    service = DictationRefinementService(refiner)
    client.app.state.dictation_refinement = service
    try:
        with caplog.at_level(logging.INFO):
            response = client.post(PATH, json={"text": text}, headers=auth)
    finally:
        service.close()
    assert response.status_code == 502
    rejected = [
        record for record in caplog.records if record.message == "dictation_refinement_rejected"
    ]
    assert len(rejected) == 1
    assert rejected[0].category == category
    completed = [
        record for record in caplog.records if record.message == "dictation_refinement_completed"
    ]
    assert len(completed) == 1
    assert completed[0].outcome == "unusable_provider_result"
    assert completed[0].elapsed_ms >= 0
    for record in [*rejected, *completed]:
        assert record.exc_info is None
        assert text not in str(record.__dict__)
        assert "PRIVATE_PROVIDER_BODY" not in str(record.__dict__)
    refiner.propose_edits.assert_called_once()


@pytest.mark.parametrize("changed", [False, True])
def test_success_diagnostics_report_outcome_without_transcripts(
    caplog: pytest.LogCaptureFixture, changed: bool
) -> None:
    refiner = Mock(spec=FakeDictationRefiner)
    refiner.propose_edits.return_value = edits(("current", " is is", " is")) if changed else edits()
    service = DictationRefinementService(refiner)
    try:
        with caplog.at_level(logging.INFO):
            asyncio.run(service.refine("This is is a test."))
    finally:
        service.close()
    record = next(r for r in caplog.records if r.message == "dictation_refinement_completed")
    assert record.outcome == ("changed" if changed else "unchanged")
    assert record.elapsed_ms >= 0
    assert "This is" not in str(record.__dict__)


@pytest.mark.parametrize("cancel", [False, True])
def test_capacity_is_held_until_underlying_work_finishes(
    cancel: bool, caplog: pytest.LogCaptureFixture
) -> None:
    release = Event()
    started = [Event() for _ in range(MAX_CONCURRENT_CALLS)]
    refiner = Mock(spec=FakeDictationRefiner)

    def block(text: str, *, previous_text: str) -> str:
        started[int(text)].set()
        assert release.wait(5)
        return '{"edits":[]}'

    refiner.propose_edits.side_effect = block
    service = DictationRefinementService(refiner, timeout_seconds=0.08)

    async def run() -> None:
        tasks = [asyncio.create_task(service.refine(str(i))) for i in range(MAX_CONCURRENT_CALLS)]
        for event in started:
            assert await asyncio.to_thread(event.wait, 2)
        # The event loop remains responsive while both synchronous calls are blocked.
        await asyncio.sleep(0)
        if cancel:
            for task in tasks:
                task.cancel()
        for task in tasks:
            with pytest.raises(asyncio.CancelledError if cancel else GatewayTimeoutError):
                await task
        for _ in range(3):
            with pytest.raises(TooManyRequestsError):
                await service.refine("extra")
        assert refiner.propose_edits.call_count == 2
        release.set()
        for _ in range(200):
            try:
                assert (await service.refine("0")).text == "0"
                break
            except TooManyRequestsError:
                await asyncio.sleep(0.005)
        else:
            pytest.fail("Capacity was not released")

    try:
        with caplog.at_level(logging.INFO):
            asyncio.run(run())
    finally:
        release.set()
        service.close()
    refiner.close.assert_called_once()
    outcomes = {
        record.outcome
        for record in caplog.records
        if record.message == "dictation_refinement_completed"
    }
    assert {"capacity", "cancelled" if cancel else "timeout", "unchanged"} <= outcomes


def test_real_endpoint_deadline_does_not_block_health_or_transcription(
    client: TestClient, auth: dict[str, str]
) -> None:
    release = Event()
    refiner = Mock(spec=FakeDictationRefiner)

    def block(text: str, *, previous_text: str) -> str:
        assert release.wait(5)
        return '{"edits":[]}'

    refiner.propose_edits.side_effect = block
    service = DictationRefinementService(refiner, timeout_seconds=0.08)
    client.app.state.dictation_refinement = service

    async def run() -> None:
        async with httpx.AsyncClient(
            transport=httpx.ASGITransport(app=client.app), base_url="http://test"
        ) as api:
            start = perf_counter()
            results = await asyncio.gather(
                api.post(PATH, json={"text": "hello"}, headers=auth),
                api.post(PATH, json={"text": "hello"}, headers=auth),
            )
            assert perf_counter() - start < 1
            assert all(response.status_code == 504 for response in results)
            assert all(response.headers["cache-control"] == "no-store" for response in results)
            response = await api.post(PATH, json={"text": "hello"}, headers=auth)
            assert response.status_code == 429
            assert response.headers["retry-after"] == "1"
            assert (await api.get("/health/live")).status_code == 200
            response = await api.post(
                "/v1/dictation/transcribe",
                content=make_wav(),
                headers={**auth, "Content-Type": "audio/wav"},
            )
            assert response.status_code == 200

    try:
        asyncio.run(run())
    finally:
        release.set()
        service.close()


def test_shutdown_drains_before_closing_adapter() -> None:
    service = DictationRefinementService(FakeDictationRefiner())
    service.close()
    with pytest.raises(ServiceUnavailableError):
        asyncio.run(service.refine("hello"))


def test_app_closes_injected_refiner(
    context: ServiceContext, verifier: StaticTokenVerifier
) -> None:
    refiner = Mock(spec=FakeDictationRefiner)
    with TestClient(create_app(context=context, verifier=verifier, dictation_refiner=refiner)):
        refiner.close.assert_not_called()
    refiner.close.assert_called_once()

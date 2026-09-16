"""Schema-driven contract tests: real API responses validated against the committed spec.

These tests read ``openapi/voice-recorder.yaml`` and validate live responses from the
FastAPI app against it, so a drift between the wire models and the published contract
(for example ``failure_reason`` being emitted as ``null`` while the schema forbids it)
fails the build instead of shipping.
"""

from __future__ import annotations

import uuid
from pathlib import Path
from typing import Any

import pytest
import yaml
from fastapi.testclient import TestClient

from voice_recorder.services.context import ServiceContext
from voice_recorder.services.pipeline import finalize

from .conftest import chunk_headers, drain, make_wav, unique_wav
from .jsonschema_lite import (
    assert_keywords_supported,
    assert_refs_resolve,
    iter_schema_nodes,
    resolve_pointer,
    validate,
)

SPEC_PATH = Path(__file__).resolve().parents[2] / "openapi" / "voice-recorder.yaml"


@pytest.fixture(scope="module")
def spec() -> dict[str, Any]:
    return yaml.safe_load(SPEC_PATH.read_text(encoding="utf-8"))


def assert_valid(payload: Any, schema_name: str, spec: dict[str, Any]) -> None:
    schema = resolve_pointer(spec, f"#/components/schemas/{schema_name}")
    errors = validate(payload, schema, spec)
    assert not errors, f"{schema_name} contract violation: " + "; ".join(errors)


# --------------------------------------------------------------------- spec health
def test_spec_is_openapi_31_and_structurally_complete(spec: dict[str, Any]) -> None:
    assert str(spec["openapi"]).startswith("3.1"), "contract tests assume OpenAPI 3.1"
    assert spec["info"]["title"] and spec["info"]["version"]
    assert spec["paths"], "spec declares no paths"
    assert spec["components"]["schemas"], "spec declares no schemas"


def test_every_ref_in_the_spec_resolves(spec: dict[str, Any]) -> None:
    assert_refs_resolve(spec)


def test_every_schema_keyword_is_covered_by_the_validator(spec: dict[str, Any]) -> None:
    # Guards the tests below: an unknown keyword must fail loudly rather than be skipped.
    assert_keywords_supported(spec)


def test_every_schema_node_declares_a_usable_shape(spec: dict[str, Any]) -> None:
    known = {"object", "array", "string", "integer", "number", "boolean", "null"}
    for pointer, schema in iter_schema_nodes(spec):
        declared = schema.get("type")
        if declared is None:
            continue
        types = declared if isinstance(declared, list) else [declared]
        assert set(types) <= known, f"{pointer}: unknown type {declared!r}"


def test_operation_ids_are_unique(spec: dict[str, Any]) -> None:
    seen: list[str] = []
    for path_item in spec["paths"].values():
        for method, operation in path_item.items():
            if method in {"get", "put", "post", "delete", "patch", "head", "options"}:
                seen.append(operation["operationId"])
    assert len(seen) == len(set(seen)), f"duplicate operationIds: {seen}"


# ----------------------------------------------------------- Recording responses
def test_created_recording_matches_schema_with_null_failure_reason(
    client: TestClient, auth: dict[str, str], spec: dict[str, Any]
) -> None:
    resp = client.post(
        "/v1/recordings", json={"client_recording_id": str(uuid.uuid4())}, headers=auth
    )
    assert resp.status_code == 201
    body = resp.json()
    assert body["failure_reason"] is None
    assert body["transcript_id"] is None
    assert_valid(body, "Recording", spec)


def test_completed_recording_matches_schema(
    client: TestClient, context: ServiceContext, auth: dict[str, str], spec: dict[str, Any]
) -> None:
    rid = client.post(
        "/v1/recordings", json={"client_recording_id": str(uuid.uuid4())}, headers=auth
    ).json()["recording_id"]
    data = unique_wav(7)
    assert (
        client.put(
            f"/v1/recordings/{rid}/chunks/0",
            content=data,
            headers={**auth, **chunk_headers(data)},
        ).status_code
        == 202
    )
    assert (
        client.post(
            f"/v1/recordings/{rid}/complete", json={"chunk_count": 1}, headers=auth
        ).status_code
        == 202
    )
    drain(context)

    body = client.get(f"/v1/recordings/{rid}", headers=auth).json()
    assert body["state"] == "completed"
    assert body["transcript_id"] is not None
    assert body["failure_reason"] is None
    assert_valid(body, "Recording", spec)


def test_failed_recording_matches_schema_with_enum_failure_reason(
    client: TestClient, context: ServiceContext, auth: dict[str, str], spec: dict[str, Any]
) -> None:
    rid = client.post(
        "/v1/recordings", json={"client_recording_id": str(uuid.uuid4())}, headers=auth
    ).json()["recording_id"]
    data = unique_wav(11)
    client.put(
        f"/v1/recordings/{rid}/chunks/0", content=data, headers={**auth, **chunk_headers(data)}
    )
    # Declare two chunks but only ever deliver one, then blow past the grace window so
    # the watchdog fails the recording with a real enum value.
    client.post(f"/v1/recordings/{rid}/complete", json={"chunk_count": 2}, headers=auth)
    drain(context)
    context.clock.advance(seconds=context.settings.chunk_grace_seconds + 1)  # type: ignore[attr-defined]
    finalize(context, rid)

    body = client.get(f"/v1/recordings/{rid}", headers=auth).json()
    assert body["state"] == "failed"
    assert body["failure_reason"] == "missing_chunks"
    assert_valid(body, "Recording", spec)


def test_failure_reason_still_rejects_values_outside_the_enum(spec: dict[str, Any]) -> None:
    schema = resolve_pointer(spec, "#/components/schemas/Recording")
    payload = {
        "recording_id": str(uuid.uuid4()),
        "client_recording_id": str(uuid.uuid4()),
        "state": "failed",
        "refine_model": "gpt-5.6-luna",
        "language": "cs",
        "progress": {
            "expected_chunk_count": 1,
            "received_chunk_count": 1,
            "transcribed_chunk_count": 0,
        },
        "transcript_id": None,
        "failure_reason": "not_a_real_reason",
        "created_at": "2026-09-03T12:00:00Z",
        "updated_at": "2026-09-03T12:00:00Z",
    }
    assert validate(payload, schema, spec), "nullable must not weaken the enum constraint"


# ------------------------------------------------------------- other wire shapes
def test_chunk_accepted_matches_schema(
    client: TestClient, auth: dict[str, str], spec: dict[str, Any]
) -> None:
    rid = client.post(
        "/v1/recordings", json={"client_recording_id": str(uuid.uuid4())}, headers=auth
    ).json()["recording_id"]
    data = unique_wav(3)
    resp = client.put(
        f"/v1/recordings/{rid}/chunks/0", content=data, headers={**auth, **chunk_headers(data)}
    )
    assert resp.status_code == 202
    assert_valid(resp.json(), "ChunkAccepted", spec)


def test_transcript_list_and_detail_match_schema(
    client: TestClient, context: ServiceContext, auth: dict[str, str], spec: dict[str, Any]
) -> None:
    rid = client.post(
        "/v1/recordings", json={"client_recording_id": str(uuid.uuid4())}, headers=auth
    ).json()["recording_id"]
    data = unique_wav(5)
    client.put(
        f"/v1/recordings/{rid}/chunks/0", content=data, headers={**auth, **chunk_headers(data)}
    )
    client.post(f"/v1/recordings/{rid}/complete", json={"chunk_count": 1}, headers=auth)
    drain(context)

    page = client.get("/v1/transcripts", headers=auth).json()
    assert_valid(page, "TranscriptListPage", spec)
    assert page["items"], "expected at least one transcript"

    detail = client.get(f"/v1/transcripts/{page['items'][0]['transcript_id']}", headers=auth).json()
    assert_valid(detail, "Transcript", spec)


def test_problem_response_matches_schema(
    client: TestClient, auth: dict[str, str], spec: dict[str, Any]
) -> None:
    resp = client.get(f"/v1/recordings/{uuid.uuid4()}", headers=auth)
    assert resp.status_code == 404
    assert_valid(resp.json(), "Problem", spec)


def test_health_responses_match_schema(client: TestClient, spec: dict[str, Any]) -> None:
    for path in ("/health/live", "/health/ready"):
        assert_valid(client.get(path).json(), "HealthStatus", spec)


def test_dictation_response_and_limits_match_schema(
    client: TestClient, auth: dict[str, str], spec: dict[str, Any]
) -> None:
    from voice_recorder.services.dictation import MAX_BODY_BYTES

    path = "/v1/dictation/transcribe"
    operation = spec["paths"][path]["post"]
    language = operation["parameters"][0]["schema"]
    assert language == {"type": "string", "enum": ["auto", "cs", "en"], "default": "auto"}
    assert operation["requestBody"]["content"]["audio/wav"]["schema"]["maxLength"] == MAX_BODY_BYTES
    generated = client.get("/openapi.json").json()["paths"][path]["post"]
    assert generated["requestBody"] == operation["requestBody"]
    assert generated["operationId"] == operation["operationId"]
    assert operation.get("security", spec["security"]) == [{"googleIdToken": []}]
    assert set(operation["responses"]) == {
        "200",
        "401",
        "403",
        "413",
        "415",
        "422",
        "429",
        "502",
        "503",
        "504",
    }
    response = client.post(path, headers={**auth, "Content-Type": "audio/wav"}, content=make_wav())
    assert response.status_code == 200
    assert_valid(response.json(), "DictationResult", spec)
    assert (
        response.headers["Cache-Control"]
        == (operation["responses"]["200"]["headers"]["Cache-Control"]["schema"]["const"])
    )
    for content, status in [(b"", 422), (bytes(MAX_BODY_BYTES + 1), 413)]:
        response = client.post(path, headers={**auth, "Content-Type": "audio/wav"}, content=content)
        assert response.status_code == status
        assert_valid(response.json(), "Problem", spec)


def test_dictation_refinement_contract(
    client: TestClient, auth: dict[str, str], spec: dict[str, Any]
) -> None:
    from voice_recorder.models import DictationRefineRequest
    from voice_recorder.services.dictation_refinement import MAX_BODY_BYTES

    path = "/v1/dictation/refine"
    operation = spec["paths"][path]["post"]
    schema = spec["components"]["schemas"]["DictationRefineRequest"]
    actual = DictationRefineRequest.model_json_schema()
    assert {key: value for key, value in actual.items() if key != "title"} == {
        **schema,
        "properties": {
            name: {**value, "title": actual["properties"][name]["title"]}
            for name, value in schema["properties"].items()
        },
    }
    generated = client.get("/openapi.json").json()["paths"][path]["post"]
    assert generated["requestBody"]["content"]["application/json"]["schema"] == actual
    assert generated["operationId"] == operation["operationId"]
    assert (
        generated["responses"]["200"]["content"]["application/json"]["schema"]
        == operation["responses"]["200"]["content"]["application/json"]["schema"]
        == {"$ref": "#/components/schemas/DictationRefineResult"}
    )
    generated_schemas = client.get("/openapi.json").json()["components"]["schemas"]
    for name in ("DictationEdit", "DictationRefineResult"):
        response_schema = generated_schemas[name]
        assert {key: value for key, value in response_schema.items() if key != "title"} == {
            **spec["components"]["schemas"][name],
            "properties": {
                field: {**field_schema, "title": response_schema["properties"][field]["title"]}
                for field, field_schema in spec["components"]["schemas"][name]["properties"].items()
            },
        }
    assert operation.get("security", spec["security"]) == [{"googleIdToken": []}]
    assert set(operation["responses"]) == {
        "200",
        "401",
        "403",
        "413",
        "415",
        "422",
        "429",
        "502",
        "503",
        "504",
    }
    for payload in [{"text": "Hello."}, {"text": "Ahoj.", "previous_text": "Earlier."}]:
        assert_valid(payload, "DictationRefineRequest", spec)
        response = client.post(path, json=payload, headers=auth)
        assert response.status_code == 200
        assert_valid(response.json(), "DictationRefineResult", spec)
        assert response.json()["edits"] == []
        assert response.headers["cache-control"] == "no-store"
    for payload in [
        {"text": ""},
        {"text": " \n"},
        {"text": "a" * 4001},
        {"text": "a", "previous_text": "b" * 8001},
        {"text": "a", "extra": True},
    ]:
        assert validate(payload, schema, spec)
        response = client.post(path, json=payload, headers=auth)
        assert response.status_code == 422
        assert_valid(response.json(), "Problem", spec)
    response = client.post(
        path,
        content=b" " * (MAX_BODY_BYTES + 1),
        headers={**auth, "Content-Type": "application/json"},
    )
    assert response.status_code == 413
    assert_valid(response.json(), "Problem", spec)


def test_dictation_edit_response_contract_rejects_missing_or_unbounded_edits(
    spec: dict[str, Any],
) -> None:
    schema = spec["components"]["schemas"]["DictationRefineResult"]
    for response in [
        {"text": "hello"},
        {"text": "hello", "edits": [{"original": "", "replacement": "hello"}]},
        {"text": "hello", "edits": [{"original": "a" * 257, "replacement": "hello"}]},
        {"text": "hello", "edits": [{"original": "helo", "replacement": "b" * 257}]},
        {"text": "hello", "edits": [{"original": "helo", "replacement": "hello", "start": 0}]},
        {"text": "hello", "edits": [{"original": "helo", "replacement": "hello"}] * 65},
    ]:
        assert validate(response, schema, spec)
    assert_valid(
        {"text": "a test.", "edits": [{"original": "This is ", "replacement": ""}]},
        "DictationRefineResult",
        spec,
    )

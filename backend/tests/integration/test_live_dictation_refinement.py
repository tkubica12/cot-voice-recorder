"""Explicitly opt-in, synthetic local API smoke test using existing Azure CLI identity."""

from __future__ import annotations

import json
import os
from time import perf_counter
from typing import Any
from unittest.mock import patch

import pytest
from fastapi.testclient import TestClient

from voice_recorder.ai.dictation_refinement import FoundryDictationRefiner
from voice_recorder.app import create_app
from voice_recorder.auth import StaticTokenVerifier
from voice_recorder.config import Settings
from voice_recorder.dictation_edits import validate_dictation_edits
from voice_recorder.services.context import ServiceContext
from voice_recorder.services.dictation_refinement import TIMEOUT_SECONDS

pytestmark = pytest.mark.skipif(
    os.environ.get("VR_RUN_LIVE_DICTATION_REFINEMENT") != "1",
    reason="Opt in with VR_RUN_LIVE_DICTATION_REFINEMENT=1; uses real Foundry inference.",
)


@pytest.mark.parametrize("case_group", ["baseline", "release-probe", "short-latency"])
def test_synthetic_local_api_with_real_luna(
    context: ServiceContext, verifier: StaticTokenVerifier, auth: dict[str, str], case_group: str
) -> None:
    from azure.identity import AzureCliCredential, get_bearer_token_provider
    from openai import AzureOpenAI

    credential = AzureCliCredential(process_timeout=15)
    credential.get_token("https://cognitiveservices.azure.com/.default")
    client = AzureOpenAI(
        azure_endpoint="https://tomaskubica-foundry-resource.cognitiveservices.azure.com/",
        api_version="2024-10-21",
        azure_ad_token_provider=get_bearer_token_provider(
            credential, "https://cognitiveservices.azure.com/.default"
        ),
        timeout=TIMEOUT_SECONDS,
        max_retries=0,
    )
    refiner = FoundryDictationRefiner(
        client, Settings(environment="test").refine_deployment_default, credential=credential
    )
    observations: list[dict[str, Any]] = []
    proposals: list[str] = []
    original_create = client.chat.completions.create

    def measured_create(**kwargs: Any) -> Any:
        result = original_create(**kwargs)
        proposals.append(result.choices[0].message.content)
        usage = result.usage
        details = usage.completion_tokens_details if usage else None
        observations.append(
            {
                "model": result.model,
                "finish_reason": result.choices[0].finish_reason,
                "completion_tokens": usage.completion_tokens if usage else None,
                "reasoning_tokens": details.reasoning_tokens if details else None,
            }
        )
        return result

    # Authentication is isolated in this local TestClient; no production auth changes.
    app = create_app(context=context, verifier=verifier, dictation_refiner=refiner)
    with (
        patch.object(client.chat.completions, "create", side_effect=measured_create),
        TestClient(app) as api,
    ):
        invalid = api.post("/v1/dictation/refine", json={"text": ""}, headers=auth)
        assert invalid.status_code == 422
        assert api.post("/v1/dictation/refine", json={"text": "Hello."}).status_code == 401
        assert not observations
        cases = [
            (
                "cs",
                "Dnes jsme byli na procházce. Počasí bylo příjemné a zítra půjdeme znovu.",
                "Mluvím o dnešním odpoledni.",
                None,
            ),
            (
                "en",
                "We recieved the package today. Please leave it beside the door.",
                "The package contains books for the library.",
                None,
            ),
            ("en-duplicate", "This is is a test.", "", "This is a test."),
            (
                "cs-duplicate",
                "Dnes jdeme do parku jdeme do parku společně.",
                "",
                "Dnes jdeme do parku společně.",
            ),
            (
                "cs-false-start",
                "Potřebujeme do- potřebujeme doplnit dokumentaci a odeslat změny.",
                "",
                "Potřebujeme doplnit dokumentaci a odeslat změny.",
            ),
            (
                "cs-repeated-lead",
                "Je to trošku spí- Trošku zpívanej, takže uvidíme.",
                "",
                "Je to trošku zpívanej, takže uvidíme.",
            ),
            (
                "cs-seam-sentence",
                "otestovat nový build a zkontrolovat logy.",
                "Před nasazením musíme otestovat nový build",
                "a zkontrolovat logy.",
            ),
            ("filler", "This is, um, a test.", "", "This is a test."),
            ("counting", "1, 1, 2, 3.", "", "1, 1, 2, 3."),
            (
                "en-seam",
                "we need to check literal123 on port 8443.",
                "First we need to check",
                "literal123 on port 8443.",
            ),
            (
                "cs-seam",
                "potřebujeme zkontrolovat literal123 na portu 8443.",
                "Nejdříve potřebujeme zkontrolovat",
                "literal123 na portu 8443.",
            ),
        ]
        duplicate_phrases = {
            "cs-release": "novou verzi aplikace",
            "en-release": "the new version",
        }
        if case_group == "release-probe":
            cases = [
                (
                    "cs-release",
                    "Dnes nasadíme novou verzi aplikace novou verzi aplikace na portu 8443. "
                    "Přihlášení ponecháme zapnuté.",
                    "",
                    "Dnes nasadíme novou verzi aplikace na portu 8443. "
                    "Přihlášení ponecháme zapnuté.",
                ),
                (
                    "en-release",
                    "We will deploy the new version the new version on port 8443. "
                    "Do not disable authentication.",
                    "",
                    "We will deploy the new version on port 8443. Do not disable authentication.",
                ),
            ]
        elif case_group == "short-latency":
            raw = (
                "Tak tady si musíme nadiktovat nějaký pěkný text, ale je taky "
                "trošku spí- Trošku zpívanej, takže bude otázka, jestli se tam "
                "budou duplikovat slova."
            )
            corrected = raw.replace("trošku spí- Trošku", "trošku")
            cases = [(f"cs-short-{index}", raw, "", corrected) for index in range(10)]
        failures: list[str] = []
        for language, text, previous, expected in cases:
            observation_start = len(observations)
            start = perf_counter()
            response = api.post(
                "/v1/dictation/refine",
                json={"text": text, "previous_text": previous},
                headers=auth,
            )
            elapsed = perf_counter() - start
            # Only timing/usage/status metadata is printed, never model or user text.
            report = {
                "language": language,
                "status": response.status_code,
                "elapsed_seconds": round(elapsed, 3),
                "changed": response.status_code == 200 and response.json().get("text") != text,
                **(observations[-1] if len(observations) > observation_start else {}),
            }
            if response.status_code != 200:
                print(json.dumps(report))
                failures.append(f"{language}: HTTP {response.status_code}")
                continue
            assert response.headers["cache-control"] == "no-store"
            body = response.json()
            assert body["text"].strip()
            assert set(body) == {"text", "edits"}
            assert all(set(edit) == {"original", "replacement"} for edit in body["edits"])
            assert len(observations) == observation_start + 1
            validated = validate_dictation_edits(text, proposals[-1], previous_text=previous)
            assert body == {
                "text": validated.text,
                "edits": [
                    {"original": edit.original, "replacement": edit.replacement}
                    for edit in validated.edits
                ],
            }
            expected_match = expected is None or body["text"] == expected
            spelling_match = language != "en" or "received" in body["text"]
            protected_literals_match = all(
                body["text"].count(literal) == text.count(literal)
                for literal in ("literal123", "8443")
            )
            duplicate_match = True
            if language in duplicate_phrases:
                occurrences = body["text"].count(duplicate_phrases[language])
                report["duplicate_occurrences"] = occurrences
                duplicate_match = occurrences == 1
            report.update(
                {
                    "edit_count": len(body["edits"]),
                    "removed_characters": len(text) - len(body["text"]),
                    "proposal_matches_response": True,
                    "expected_match": expected_match and spelling_match,
                    "protected_literals_match": protected_literals_match,
                }
            )
            print(json.dumps(report))
            if body["text"] == text:
                assert body["edits"] == []
            if (
                not expected_match
                or not spelling_match
                or not protected_literals_match
                or not duplicate_match
            ):
                failures.append(f"{language}: unexpected synthetic correction")
        assert not failures, "; ".join(failures)
        assert len(observations) == len(cases)
        assert all(value["reasoning_tokens"] == 0 for value in observations)

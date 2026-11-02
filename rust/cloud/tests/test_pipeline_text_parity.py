"""Actual Python normalized tokens, chunk seams and UUID5 publication identities."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import unicodedata
import unittest
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3].joinpath("backend", "src")))
from voice_recorder.prompts import REFINEMENT_SYSTEM_PROMPT
from voice_recorder.stitch import _normalize_token, stitch_chunks

ORACLE: Path


class PipelineTextParityTests(unittest.TestCase):
    def test_existing_recording_prompt_is_preserved_without_reusing_the_dictation_prompt(
        self,
    ) -> None:
        prompt = (
            Path(__file__)
            .resolve()
            .parents[1]
            .joinpath("resources", "recording-system.txt")
            .read_text(encoding="utf-8")
        )
        self.assertEqual(prompt.rstrip("\n"), REFINEMENT_SYSTEM_PROMPT)

    def probe(self, texts: list[str], tokens: list[str], minimum: int = 8) -> dict:
        result = subprocess.run(
            [str(ORACLE), "pipeline-text"],
            input=json.dumps(
                {
                    "texts": texts,
                    "tokens": tokens,
                    "recording_id": "synthetic-fixture",
                    "min_chars": minimum,
                },
                ensure_ascii=False,
            ).encode(),
            capture_output=True,
            timeout=120,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_all_assigned_unicode_scalars_normalize_like_the_actual_python_stitcher(
        self,
    ) -> None:
        tokens = [
            chr(value)
            for value in range(0x110000)
            if not 0xD800 <= value <= 0xDFFF
            and unicodedata.category(chr(value)) != "Cn"
        ]
        actual = self.probe([], tokens)["tokens"]
        self.assertEqual(len(actual), len(tokens))
        mismatches = [
            (hex(ord(token)), expected, found)
            for token, found in zip(tokens, actual, strict=True)
            if (expected := _normalize_token(token)) != found
        ]
        self.assertEqual(mismatches[:20], [], f"{len(mismatches)} mismatches")

    def test_actual_chunk_seams_and_stable_transcript_event_publication_ids(
        self,
    ) -> None:
        for texts in [
            ["check the Straße", "THE STRASSE next"],
            ["go now", "now go elsewhere"],
            ["prefix １２３４５６７８", "12345678 suffix"],
            ["first\x1csecond", "FIRST second third"],
            ["one ČESKÝ text", "český TEXT continues"],
            ["--- ...", "... ---", "final"],
        ]:
            actual = self.probe(texts, [])
            self.assertEqual(actual["text"], stitch_chunks(texts))
            self.assertEqual(
                actual["transcript_id"],
                str(
                    uuid.uuid5(
                        uuid.uuid5(uuid.NAMESPACE_URL, "voice-recorder/transcript"),
                        "synthetic-fixture",
                    )
                ),
            )
            self.assertEqual(
                actual["event_id"],
                str(
                    uuid.uuid5(
                        uuid.uuid5(uuid.NAMESPACE_URL, "voice-recorder/event"),
                        "synthetic-fixture",
                    )
                ),
            )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--oracle", type=Path, required=True)
    args = parser.parse_args()
    ORACLE = args.oracle.resolve(strict=True)
    unittest.main(argv=[__file__], verbosity=2)

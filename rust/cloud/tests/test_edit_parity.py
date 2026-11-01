"""Actual Python edit protection -> Rust, with deterministic synthetic corpora only."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3].joinpath("backend", "src")))
from voice_recorder.ai.dictation_refinement import SYSTEM_PROMPT
from voice_recorder.dictation_edits import (
    InvalidDictationEdits,
    validate_dictation_edits,
)

ORACLE: Path


class EditParityTests(unittest.TestCase):
    def test_server_prompt_preserves_the_existing_exact_instruction(self) -> None:
        prompt = (
            Path(__file__)
            .resolve()
            .parents[1]
            .joinpath("resources", "dictation-system.txt")
            .read_text(encoding="utf-8")
        )
        self.assertEqual(prompt.rstrip("\n"), SYSTEM_PROMPT)

    def compare(self, text: str, edits: object, previous: str = "") -> None:
        payload = json.dumps({"edits": edits}, ensure_ascii=False)
        try:
            expected = validate_dictation_edits(text, payload, previous_text=previous)
        except InvalidDictationEdits:
            expected = None
        result = subprocess.run(
            [str(ORACLE), "edits"],
            input=json.dumps(
                {"text": text, "payload": payload, "previous": previous},
                ensure_ascii=False,
            ).encode(),
            capture_output=True,
            timeout=10,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        actual = json.loads(result.stdout)
        self.assertEqual(
            actual["accepted"], expected is not None, (text, edits, actual)
        )
        if expected is not None:
            self.assertEqual(actual["text"], expected.text)
            self.assertEqual(
                actual["edits"],
                [
                    {"original": edit.original, "replacement": edit.replacement}
                    for edit in expected.edits
                ],
            )

    def test_safe_and_unsafe_anchors_numbers_identifiers_scripts_whitespace_and_echo(
        self,
    ) -> None:
        for text, edits, previous in [
            ("This is is a test.", [["current", " is is", " is"]], ""),
            ("repeat repeat", [["current", "repeat", ""]], ""),
            ("port 8443", [["current", "8443", "9443"]], ""),
            ("SDK case", [["current", "SDK", "api"]], ""),
            ("hello world", [["current", "world", "世界"]], ""),
            (" alpha beta ", [["current", " alpha ", "alpha "]], ""),
            ("one\ntwo", [["current", "one\n", "one "]], ""),
            ("This is, um, a test.", [["current", ", um,", ""]], ""),
            (
                "Potřebujeme do- potřebujeme doplnit dokumentaci.",
                [["current", "do- potřebujeme ", ""]],
                "",
            ),
            ("literal123 at 10.2.3.4", [], ""),
            (" first\x1csecond ", [], ""),
            ("first\x1csecond", [["current", "\x1c", " "]], ""),
            (
                "hello world",
                [["current", "hello", "hello"], ["current", "world", "world"]],
                "",
            ),
        ]:
            with self.subTest(text=text):
                self.compare(text, edits, previous)
        for literal in [
            "literal123",
            "8443",
            "+12.5%",
            "fooBar",
            "SDK",
            "snake_case",
            "https://example.invalid",
            "foo@example.invalid",
            r"D:\fixture",
            "1.2.3",
            "C++",
            "Ⅳ",
            "²",
            "①",
            "⑩",
            "Δ",
            "Český",
        ]:
            with self.subTest(literal=literal):
                self.compare(
                    f"prefix {literal} suffix", [["current", literal, "replacement"]]
                )
                self.compare(f"prefix {literal} suffix", [])


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--oracle", type=Path, required=True)
    args = parser.parse_args()
    ORACLE = args.oracle.resolve(strict=True)
    unittest.main(argv=[__file__], verbosity=2)

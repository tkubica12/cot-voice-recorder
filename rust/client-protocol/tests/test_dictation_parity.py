"""Actual C# text matching and legacy checkpoint parity. Synthetic inputs only."""

from __future__ import annotations

import argparse
import json
import random
import struct
import subprocess
import unittest
from pathlib import Path

CLIENT: Path
REFERENCE: Path


def run(executable: list[str], mode: str, data: bytes) -> bytes:
    result = subprocess.run(
        [*executable, mode], input=data, capture_output=True, timeout=30, check=False
    )
    if result.returncode:
        raise AssertionError(result.stderr.decode(errors="replace"))
    return result.stdout


def compare(mode: str, data: bytes, *, is_json: bool = False) -> None:
    actual = run([str(CLIENT)], mode, data)
    expected = run(["dotnet", str(REFERENCE)], mode, data)
    if is_json:
        actual, expected = json.loads(actual), json.loads(expected)
    if actual != expected:
        raise AssertionError(f"{mode} parity mismatch")


def check_backend_model() -> None:
    import sys
    from datetime import UTC, datetime, timedelta

    sys.path.insert(
        0, str(Path(__file__).resolve().parents[3].joinpath("backend", "src"))
    )
    from voice_recorder.models import TranscriptSummary

    now = datetime.now(UTC)
    summary = TranscriptSummary(
        transcript_id="synthetic-transcript",
        recording_id="synthetic-recording",
        preview="synthetic",
        completed_at=now,
        expires_at=now + timedelta(hours=48),
        language="auto",
        refine_model="none",
        source="windows_dictation",
        transcribe_model="MAI-Transcribe-2-Streaming",
    )
    data = summary.model_dump_json().encode()
    if (
        b"character_count" in data
        or run([str(CLIENT)], "summary-check", data) != b"synthetic-transcript"
    ):
        raise AssertionError("Actual backend summary/Rust client parity failed")
    print("Actual backend model -> Rust summary parity: PASS")


class DictationParityTests(unittest.TestCase):
    def test_normalization_matches_bmp_and_supplementary_compatibility_fixtures(
        self,
    ) -> None:
        words = [
            chr(value)
            for value in range(0x10000)
            if not 0xD800 <= value <= 0xDFFF
            and not 0xFDD0 <= value <= 0xFDEF
            and value not in (0xFFFE, 0xFFFF)
        ]
        words.extend(
            [
                "I",
                "\u0130",
                "\u03a3",
                "\u03bf\u03a3",
                "\u0bf0",
                "\uff21",
                "\ufb03",
                "\U00010400",
                "\U0001d400",
                "\U0001e030",
                "\u1c89",
            ]
        )
        data = json.dumps(words, ensure_ascii=True).encode()
        actual = json.loads(run([str(CLIENT)], "normalize-batch", data))
        expected = json.loads(run(["dotnet", str(REFERENCE)], "normalize-batch", data))
        differences = [
            (words[index], left, right)
            for index, (left, right) in enumerate(zip(actual, expected, strict=True))
            if left != right
        ]
        self.assertEqual(
            differences[:20], [], "Unicode data/casing does not match the reference"
        )

    def test_stitch_keeps_empty_adjacency_short_words_and_normal_pause_repetitions(
        self,
    ) -> None:
        fixtures = [
            [("go to", False), ("to the store", True)],
            [("that", False), ("that matters", True)],
            [("again", False), ("again", False)],
            [("again", False), ("", False), ("again", True)],
            [("!!!", False), ("!!! okay", True)],
            [("\u010cesk\u00fd text", False), ("TEXT pokra\u010duje", True)],
            [("number \uff21", False), ("a next", True)],
        ]
        for chunks in fixtures:
            compare(
                "stitch",
                json.dumps(
                    [{"text": text, "overlap": overlap} for text, overlap in chunks]
                ).encode(),
            )

    def test_append_preserves_exact_prefix_and_only_immediate_segment_overlap(
        self,
    ) -> None:
        for previous, incoming, overlap, count in [
            ("exact \r\n prefix", "prefix next", True, None),
            ("one two", "two next", True, 0),
            ("exact prefix", "  ", False, None),
            ("old that", "that works", True, 1),
        ]:
            compare(
                "append-prefix",
                json.dumps(
                    {
                        "previous": previous,
                        "incoming": incoming,
                        "overlap": overlap,
                        "previous_words": count,
                    }
                ).encode(),
            )

    def test_legacy_audio_snapshot_json_matches_actual_csharp_byte_arrays_and_cursor(
        self,
    ) -> None:
        generator = random.Random(2026)
        for size in [0, 17, 640, 6400, 191999, 200000]:
            pcm = b"".join(
                struct.pack("<h", generator.randrange(-1000, 1000))
                for _ in range(size // 2)
            )
            if size % 2:
                pcm += b"\x01"
            compare("legacy-state", pcm, is_json=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--client", type=Path, required=True)
    parser.add_argument("--reference", type=Path, required=True)
    parser.add_argument(
        "--backend-model",
        action="store_true",
        help="Also use the installed backend Python dependencies; not run by Rust-only CI",
    )
    args = parser.parse_args()
    CLIENT = args.client.resolve(strict=True)
    REFERENCE = args.reference.resolve(strict=True)
    if args.backend_model:
        check_backend_model()
    unittest.main(argv=[__file__], verbosity=2)

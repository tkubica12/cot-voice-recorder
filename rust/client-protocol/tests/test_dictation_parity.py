"""Actual C# text matching and legacy checkpoint parity. Synthetic inputs only."""

from __future__ import annotations

import argparse
import array
import hashlib
import json
import random
import struct
import subprocess
import unittest
from pathlib import Path

CLIENT: Path
REFERENCE: Path
LEGACY_ORACLE_OUTPUT: Path | None = None


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
    def test_legacy_functional_600s_sample_volume_oracle_matches_csharp_without_wallclock_capture(self) -> None:
        samples = array.array("h", (
            1200 + index % 257 - 128
            if 9600 <= index % 192000 < 137600 or index % 192000 >= 147200
            else index % 97 - 48
            for index in range(600 * 16000)
        ))
        import sys
        if sys.byteorder != "little":
            samples.byteswap()
        pcm = samples.tobytes()
        self.assertEqual(len(pcm), 19_200_000)
        rust = run([str(CLIENT.with_name("audio_contract.exe")), "chunk"], "1280", pcm)
        csharp = run(["dotnet", str(REFERENCE), "chunk"], "1280", pcm)
        self.assertEqual(rust, csharp)
        count = struct.unpack_from("<I", rust)[0]
        position = 4
        chunks = []
        for _ in range(count):
            end, overlap, length = struct.unpack_from("<Q?I", rust, position)
            position += 13
            wav = rust[position:position + length]
            position += length
            chunks.append({"end_byte": end, "overlaps_previous": overlap,
                           "wav_bytes": length, "digest": hashlib.sha256(wav).hexdigest()})
        self.assertEqual(position, len(rust))
        self.assertEqual(chunks[-1]["end_byte"], len(pcm))
        self.assertTrue(any(chunk["wav_bytes"] == 192044 for chunk in chunks))
        self.assertTrue(any(chunk["overlaps_previous"] for chunk in chunks))
        self.assertTrue(any(chunk["end_byte"] < len(pcm) and chunk["wav_bytes"] < 192044 for chunk in chunks))
        self.assertLess(chunks[-1]["wav_bytes"], 192044)
        self.assertNotEqual(sum(chunk["wav_bytes"] - 44 for chunk in chunks), len(pcm))
        self.assertEqual(len({chunk["digest"] for chunk in chunks}), len(chunks))
        if LEGACY_ORACLE_OUTPUT is not None:
            with LEGACY_ORACLE_OUTPUT.open("x", encoding="utf-8") as output:
                json.dump({"scope": "offline-sample-volume-not-actual600-capture",
                           "engine": "legacy", "pattern": "voiced-pause-overlap-v1",
                           "audio_seconds": 600, "raw_bytes": len(pcm),
                           "actual_rust_csharp_wav_bytes_equal": True, "chunks": chunks}, output, indent=2)

    def test_restored_maximum_four_chunk_legacy_batch_matches_actual_csharp(self) -> None:
        prefix = struct.pack("<h", 1200) * 95680
        raw = b"".join(
            struct.pack("<h", 0 if 52 <= frame <= 71 or 152 <= frame <= 171 else 1200) * 320
            for frame in range(1, 201)
        )
        self.assertEqual((len(prefix), len(raw)), (191360, 128000))
        compare("legacy-state", prefix, is_json=True)
        audio = CLIENT.with_name("audio_contract.exe")
        for fragment in [640, 1280, 128000]:
            rust = run([str(audio), "chunk", str(fragment)], "191360", prefix + raw)
            csharp = run(["dotnet", str(REFERENCE), "chunk", str(fragment)], "191360", prefix + raw)
            self.assertEqual(rust, csharp)
            self.assertEqual(struct.unpack_from("<I", rust)[0], 4)
            position = 4
            ends = []
            overlaps = []
            for _ in range(4):
                end, overlap, length = struct.unpack_from("<Q?I", rust, position)
                position += 13
                self.assertLessEqual(length, 192044)
                self.assertEqual(rust[position:position + 4], b"RIFF")
                position += length
                ends.append(end)
                overlaps.append(overlap)
            self.assertEqual(position, len(rust))
            self.assertEqual(ends, [192000, 236800, 300800, 319360])
            self.assertEqual(overlaps, [False, True, False, False])

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
        for size in [0, 17, 640, 6400, 64001, 191999, 200000]:
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
    parser.add_argument("--legacy-oracle-output", type=Path)
    parser.add_argument(
        "--backend-model",
        action="store_true",
        help="Also use the installed backend Python dependencies; not run by Rust-only CI",
    )
    args = parser.parse_args()
    CLIENT = args.client.resolve(strict=True)
    REFERENCE = args.reference.resolve(strict=True)
    LEGACY_ORACLE_OUTPUT = args.legacy_oracle_output
    if args.backend_model:
        check_backend_model()
    unittest.main(argv=[__file__], verbosity=2)

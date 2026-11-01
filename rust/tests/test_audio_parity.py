"""Compare Rust directly with the current C# chunker and Python WAV validator."""

from __future__ import annotations

import argparse
import random
import struct
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT.joinpath("backend", "src")))

from voice_recorder.problems import ValidationProblemError  # noqa: E402
from voice_recorder.wav import parse_wav, validate_wav  # noqa: E402

RUST_ORACLE: Path
DOTNET_ORACLE: Path
CAPTURE_FORMAT = {"sample_rate": 16000, "channels": 1, "bits_per_sample": 16}


def pcm(milliseconds: int, sample: int = 1200) -> bytes:
    return struct.pack("<h", sample) * (milliseconds * 16)


def wav(
    audio: bytes,
    *,
    audio_format: int = 1,
    channels: int = 1,
    rate: int = 16000,
    bits: int = 16,
) -> bytes:
    alignment = channels * bits // 8
    fmt = struct.pack(
        "<4sIHHIIHH", b"fmt ", 16, audio_format, channels, rate, rate * alignment, alignment, bits
    )
    data = struct.pack("<4sI", b"data", len(audio)) + audio
    return struct.pack("<4sI4s", b"RIFF", 4 + len(fmt) + len(data), b"WAVE") + fmt + data


def riff(chunks: bytes) -> bytes:
    return struct.pack("<4sI4s", b"RIFF", 4 + len(chunks), b"WAVE") + chunks


def run_rust(args: list[str], data: bytes) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        [str(RUST_ORACLE), *args], input=data, capture_output=True, timeout=30, check=False
    )


def run_dotnet(args: list[str], data: bytes) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        ["dotnet", str(DOTNET_ORACLE), *args],
        input=data,
        capture_output=True,
        timeout=30,
        check=False,
    )


class AudioParityTests(unittest.TestCase):
    def test_wav_encoding_matches_csharp_and_python_bytes(self) -> None:
        generator = random.Random(20261101)
        for data in [pcm(1), pcm(111, -32768), pcm(6000, 32767), generator.randbytes(640)]:
            with self.subTest(size=len(data)):
                rust = run_rust(["wrap"], data)
                csharp = run_dotnet(["wrap"], data)
                self.assertEqual(rust.returncode, 0, rust.stderr)
                self.assertEqual(csharp.returncode, 0, csharp.stderr)
                self.assertEqual(rust.stdout, csharp.stdout)
                self.assertEqual(rust.stdout, wav(data))

    def test_invalid_pcm_is_rejected_by_both_encoders(self) -> None:
        for data in [b"", b"\x01", b"\x01\x02\x03"]:
            with self.subTest(data=data):
                self.assertEqual(run_rust(["wrap"], data).returncode, 2)
                self.assertEqual(run_dotnet(["wrap"], data).returncode, 2)

    def test_chunking_and_snapshots_match_actual_csharp_implementation(self) -> None:
        generator = random.Random(42)
        patterned = b"".join(struct.pack("<h", 1000 + index * 2 % 20000) for index in range(224000))
        quiet = pcm(300000, 0)
        inputs = [
            pcm(111),
            pcm(1600) + pcm(400, 0) + pcm(300),
            pcm(6000),
            pcm(6000) + pcm(5000, 0) + pcm(300),
            pcm(1000, 0) + patterned + pcm(500, 0),
            generator.randbytes(32000),
            quiet,
        ]
        for data in inputs:
            boundaries = [0, 1, 37, len(data) // 2, len(data)]
            if len(data) > 300007:
                boundaries.extend([32001, 191997, 224000, 300007])
            for fragment in [37, 1280, 32001]:
                for boundary in [None, *boundaries]:
                    with self.subTest(size=len(data), fragment=fragment, boundary=boundary):
                        args = ["chunk", str(fragment)]
                        if boundary is not None:
                            args.append(str(boundary))
                        rust = run_rust(args, data)
                        csharp = run_dotnet(args, data)
                        self.assertEqual(rust.returncode, 0, rust.stderr)
                        self.assertEqual(csharp.returncode, 0, csharp.stderr)
                        self.assertEqual(rust.stdout, csharp.stdout)

    def test_parser_and_capture_validation_match_python(self) -> None:
        original = wav(pcm(20))
        fmt = original[12:36]
        data = original[36:]
        cases = [
            b"",
            b"RIFF",
            original,
            wav(pcm(20), audio_format=3),
            wav(pcm(20), channels=2),
            wav(pcm(20), rate=8000),
            wav(pcm(20), bits=8),
            wav(pcm(20), channels=2, rate=8000, bits=8),
            wav(pcm(20), channels=0),
            wav(pcm(20), bits=0),
            wav(b""),
            wav(b"\x01"),
            original[:-2],
            original + b"\x01",
            riff(fmt + fmt + data),
            riff(data + fmt),
            riff(fmt),
            riff(data),
            riff(b"JUNK" + struct.pack("<I", 3) + b"abc\0" + fmt + data),
            riff(b"fmt " + struct.pack("<I", 18) + fmt[8:] + b"\0\0" + data),
            riff(b"fmt " + struct.pack("<I", 17) + fmt[8:] + b"\0\0" + data),
            riff(fmt + data + data),
        ]
        for offset, replacement in [
            (0, b"FAIL"),
            (8, b"FAIL"),
            (4, bytes(4)),
            (16, struct.pack("<I", 0xFFFFFFFF)),
            (28, bytes(4)),
            (32, bytes(2)),
            (40, struct.pack("<I", 0xFFFFFFFF)),
        ]:
            modified = bytearray(original)
            modified[offset : offset + len(replacement)] = replacement
            cases.append(bytes(modified))
        generator = random.Random(100)
        for _ in range(100):
            modified = bytearray(original)
            offset = generator.randrange(len(modified))
            modified[offset] = generator.randrange(256)
            cases.append(bytes(modified))
        for case_index, candidate in enumerate(cases):
            for strict in [False, True]:
                for validate in [False, True]:
                    with self.subTest(case=case_index, strict=strict, validate=validate):
                        args = [
                            "validate" if validate else "parse",
                            "strict" if strict else "legacy",
                        ]
                        rust = run_rust(args, candidate)
                        try:
                            expected = (
                                validate_wav(candidate, strict=strict, **CAPTURE_FORMAT)
                                if validate
                                else parse_wav(candidate, strict=strict)
                            )
                        except ValidationProblemError as error:
                            self.assertEqual(rust.returncode, 2, rust.stdout)
                            diagnostic = rust.stderr.decode("utf-8")
                            for field_error in error.errors or []:
                                if field_error["field"] == "body":
                                    self.assertIn(field_error["message"], diagnostic)
                                else:
                                    self.assertIn(field_error["field"] + ":", diagnostic)
                        else:
                            self.assertEqual(rust.returncode, 0, rust.stderr)
                            actual = tuple(
                                map(int, rust.stdout.decode("ascii").strip().split("\t"))
                            )
                            self.assertEqual(
                                actual,
                                (
                                    expected.audio_format,
                                    expected.channels,
                                    expected.sample_rate,
                                    expected.bits_per_sample,
                                    expected.data_bytes,
                                ),
                            )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust-oracle", type=Path, required=True)
    parser.add_argument("--dotnet-oracle", type=Path, required=True)
    arguments = parser.parse_args()
    RUST_ORACLE = arguments.rust_oracle.resolve(strict=True)
    DOTNET_ORACLE = arguments.dotnet_oracle.resolve(strict=True)
    unittest.main(argv=[sys.argv[0]], verbosity=2)

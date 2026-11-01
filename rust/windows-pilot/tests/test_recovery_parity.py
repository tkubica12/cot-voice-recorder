"""Recovery manifest v1-v4 parity against the actual C# store; synthetic data only."""

from __future__ import annotations

import argparse
import hashlib
import json
import struct
import subprocess
import tempfile
import unittest
from pathlib import Path

PILOT: Path
REFERENCE: Path
ID = "0123456789abcdef0123456789abcdef"
TICKS = 621_355_968_000_000_000


def run(command: list[str], data: bytes = b"") -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        command, input=data, capture_output=True, timeout=20, check=False
    )


def checked(command: list[str], data: bytes = b"") -> bytes:
    result = run(command, data)
    if result.returncode:
        raise AssertionError(result.stderr.decode(errors="replace"))
    return result.stdout


def field(data: bytes) -> bytes:
    return struct.pack("<i", len(data)) + data


def legacy(version: int, audio: bytes) -> bytes:
    return b"".join(
        [
            struct.pack("<ii", 0x5650524A, version),
            field(ID.encode()),
            struct.pack("<qq", TICKS, TICKS + 1),
            field(b"auto"),
            struct.pack("<q", len(audio) - 44),
            field(b""),
            b"\x01",
            struct.pack("<i", 1),
            struct.pack("<i", 0),
            b"\x00\x00",
            struct.pack("<i", len(audio)),
            field(hashlib.sha256(audio).digest()),
            struct.pack("<q", len(audio) - 44) if version >= 3 else b"",
            field(b"synthetic context") if version >= 2 else b"",
        ]
    )


class RecoveryParityTests(unittest.TestCase):
    def test_actual_csharp_v3_v4_journals_roundtrip_through_rust_byte_exactly(
        self,
    ) -> None:
        for mode in ["legacy", "streaming", "audit"]:
            with (
                self.subTest(mode=mode),
                tempfile.TemporaryDirectory(
                    prefix="VoicePrompt-recovery-current-"
                ) as directory,
            ):
                before = json.loads(
                    checked(
                        ["dotnet", str(REFERENCE), "recovery-create", directory, mode]
                    )
                )
                path = Path(directory).joinpath(before["Id"], "manifest.enc")
                original_plain = checked(
                    ["dotnet", str(REFERENCE), "unprotect"], path.read_bytes()
                )
                rust_plain = checked([str(PILOT), "--recovery-load-fixture", str(path)])
                self.assertEqual(rust_plain, original_plain)
                checked([str(PILOT), "--recovery-save-fixture", str(path)], rust_plain)
                after = json.loads(
                    checked(
                        [
                            "dotnet",
                            str(REFERENCE),
                            "recovery-inspect",
                            directory,
                            before["Id"],
                        ]
                    )
                )
                self.assertEqual(before, after)
                self.assertFalse(any(path.parent.glob("*.tmp")))
                self.assertNotIn(b"synthetic", path.read_bytes())

    def test_old_v1_v2_manifests_preserve_unknown_cursors_and_context(self) -> None:
        audio = checked(["dotnet", str(REFERENCE), "wrap"], bytes([1]) * 6400)
        for version in [1, 2]:
            with (
                self.subTest(version=version),
                tempfile.TemporaryDirectory(
                    prefix="VoicePrompt-recovery-legacy-"
                ) as directory,
            ):
                session = Path(directory).joinpath(ID)
                session.mkdir()
                path = session.joinpath("manifest.enc")
                path.write_bytes(
                    checked([str(PILOT), "--protect-stdin"], legacy(version, audio))
                )
                session.joinpath("00000000.wav.enc").write_bytes(
                    checked([str(PILOT), "--protect-stdin"], audio)
                )
                before = json.loads(
                    checked(
                        ["dotnet", str(REFERENCE), "recovery-inspect", directory, ID]
                    )
                )
                self.assertFalse(before["Streaming"])
                self.assertEqual(before["Chunks"][0]["EndByte"], 0)
                self.assertEqual(
                    before["Context"], "" if version == 1 else "synthetic context"
                )
                latest = checked([str(PILOT), "--recovery-load-fixture", str(path)])
                self.assertEqual(struct.unpack_from("<i", latest, 4)[0], 3)
                checked([str(PILOT), "--recovery-save-fixture", str(path)], latest)
                after = json.loads(
                    checked(
                        ["dotnet", str(REFERENCE), "recovery-inspect", directory, ID]
                    )
                )
                self.assertEqual(before, after)

    def test_corrupt_plaintext_never_replaces_the_existing_encrypted_manifest(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(
            prefix="VoicePrompt-recovery-invalid-"
        ) as directory:
            state = json.loads(
                checked(
                    [
                        "dotnet",
                        str(REFERENCE),
                        "recovery-create",
                        directory,
                        "streaming",
                    ]
                )
            )
            path = Path(directory).joinpath(state["Id"], "manifest.enc")
            before = path.read_bytes()
            for invalid in [b"corrupt", b"", bytes([0]) * 120]:
                refused = run(
                    [str(PILOT), "--recovery-save-fixture", str(path)], invalid
                )
                self.assertNotEqual(refused.returncode, 0)
                self.assertEqual(path.read_bytes(), before)
            path.write_bytes(b"corrupt synthetic ciphertext")
            refused = run([str(PILOT), "--recovery-load-fixture", str(path)])
            self.assertNotEqual(refused.returncode, 0)
            self.assertEqual(path.read_bytes(), b"corrupt synthetic ciphertext")

    def test_fixture_paths_cannot_escape_the_recovery_layout(self) -> None:
        with tempfile.TemporaryDirectory(
            prefix="VoicePrompt-recovery-paths-"
        ) as directory:
            for path in [
                Path(directory).joinpath("manifest.enc"),
                Path(directory).joinpath(ID.upper(), "manifest.enc"),
                Path(directory).joinpath(ID, "unexpected.bin"),
            ]:
                refused = run(
                    [str(PILOT), "--recovery-save-fixture", str(path)], b"synthetic"
                )
                self.assertNotEqual(refused.returncode, 0)
                self.assertFalse(path.exists())
            self.assertEqual(list(Path(directory).iterdir()), [])


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pilot", type=Path, required=True)
    parser.add_argument("--reference", type=Path, required=True)
    args = parser.parse_args()
    PILOT = args.pilot.resolve(strict=True)
    REFERENCE = args.reference.resolve(strict=True)
    unittest.main(argv=[__file__], verbosity=2)

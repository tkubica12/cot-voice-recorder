"""Frozen C# v1-v4 recovery wires and independent OS protection; synthetic only."""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "tests"))
from legacy_reference import invoke

PILOT: Path
REFERENCE: Path
ID = "0123456789abcdef0123456789abcdef"
TICKS = 621_355_968_000_000_000


def run(command: list[str], data: bytes = b"") -> subprocess.CompletedProcess[bytes]:
    if command[:2] == ["dotnet", str(REFERENCE)]:
        return invoke(command[2:], data, REFERENCE)
    return subprocess.run(command, input=data, capture_output=True, timeout=20, check=False)


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
    def test_junctions_are_rejected_before_read_write_or_delete(self) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-recovery-junction-") as directory:
            root = Path(directory).joinpath("root")
            target = Path(directory).joinpath("target")
            root.mkdir()
            target.mkdir()
            sentinel = target.joinpath("manifest.enc")
            sentinel.write_bytes(b"synthetic sentinel must not be changed")
            junction = root.joinpath(ID)
            environment = dict(os.environ)
            environment["VOICEPROMPT_FIXTURE_LINK"] = str(junction)
            environment["VOICEPROMPT_FIXTURE_TARGET"] = str(target)
            created = subprocess.run(
                [
                    shutil.which("pwsh") or "powershell.exe",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    (
                        "$ErrorActionPreference='Stop'; "
                        "New-Item -ItemType Junction -Path $env:VOICEPROMPT_FIXTURE_LINK "
                        "-Target $env:VOICEPROMPT_FIXTURE_TARGET | Out-Null"
                    ),
                ],
                env=environment,
                capture_output=True,
                timeout=60,
                check=False,
            )
            self.assertEqual(created.returncode, 0, created.stderr)
            self.assertTrue(os.path.isjunction(junction))
            try:
                for command, data in [
                    (
                        [str(PILOT), "--recovery-open-journal-fixture", str(root), ID],
                        b"",
                    ),
                    (
                        [str(PILOT), "--recovery-delete-idle-fixture", str(root), ID],
                        b"",
                    ),
                    (
                        [
                            str(PILOT),
                            "--recovery-save-fixture",
                            str(junction.joinpath("manifest.enc")),
                        ],
                        b"corrupt",
                    ),
                    (
                        [str(PILOT), "--recovery-root-lock-fixture", str(junction)],
                        b"\n",
                    ),
                ]:
                    refused = run(command, data)
                    self.assertNotEqual(refused.returncode, 0)
                    self.assertIn(b"reparse", refused.stderr.lower())
                    self.assertEqual(
                        sentinel.read_bytes(), b"synthetic sentinel must not be changed"
                    )
            finally:
                junction.rmdir()
            self.assertTrue(target.is_dir())

    def test_native_durable_journals_are_readable_by_the_actual_csharp_store(
        self,
    ) -> None:
        audio = checked(["dotnet", str(REFERENCE), "wrap"], bytes([1]) * 6400)
        for mode in ["legacy", "streaming", "audit"]:
            with (
                self.subTest(mode=mode),
                tempfile.TemporaryDirectory(prefix="VoicePrompt-recovery-native-") as directory,
            ):
                plain = checked([str(PILOT), "--recovery-journal-fixture", directory, mode], audio)
                sessions = [path for path in Path(directory).iterdir() if path.is_dir()]
                self.assertEqual(len(sessions), 1)
                session = sessions[0]
                current = checked(
                    ["dotnet", str(REFERENCE), "unprotect"],
                    session.joinpath("manifest.enc").read_bytes(),
                )
                self.assertEqual(plain, current)
                state = json.loads(
                    checked(
                        [
                            "dotnet",
                            str(REFERENCE),
                            "recovery-inspect",
                            directory,
                            session.name,
                        ]
                    )
                )
                self.assertEqual(state["CapturedBytes"], 12800)
                self.assertEqual(state["Context"], "synthetic context")
                self.assertEqual(state["Streaming"], mode != "legacy")
                self.assertTrue(state["CaptureComplete"])
                self.assertEqual(
                    state["Chunks"][0]["Text"],
                    "synthetic first" if mode == "legacy" else "",
                )
                if mode == "audit":
                    self.assertEqual(state["ConfirmedBytes"], 12800)
                    self.assertEqual(state["AuditText"], "synthetic polished final")
                    self.assertFalse(session.joinpath("00000001.wav.enc").exists())
                else:
                    self.assertIsNone(state["Chunks"][1]["Text"])
                    self.assertTrue(session.joinpath("00000001.wav.enc").exists())
                self.assertEqual(Path(directory).joinpath("writer.lock").stat().st_size, 0)

    def test_writer_ownership_is_enforced_between_real_native_processes(self) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-recovery-lock-") as directory:
            owner = subprocess.Popen(
                [str(PILOT), "--recovery-root-lock-fixture", directory],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            try:
                assert owner.stdout is not None
                with concurrent.futures.ThreadPoolExecutor(max_workers=1) as executor:
                    ready = executor.submit(owner.stdout.readline).result(timeout=15)
                self.assertEqual(ready, b"ready\n")
                self.assertIsNone(owner.poll())
                refused = run([str(PILOT), "--recovery-root-lock-fixture", directory], b"\n")
                self.assertNotEqual(refused.returncode, 0)
                self.assertIn(b"exclusive recovery writer", refused.stderr)
                _, error = owner.communicate(b"\n", timeout=15)
                self.assertEqual(owner.returncode, 0, error)
                self.assertEqual(
                    checked([str(PILOT), "--recovery-root-lock-fixture", directory], b"\n"),
                    b"ready\n",
                )
            finally:
                if owner.poll() is None:
                    owner.kill()
                owner.communicate(timeout=15)

    def test_actual_csharp_v3_v4_journals_roundtrip_through_rust_byte_exactly(
        self,
    ) -> None:
        for mode in ["legacy", "streaming", "audit"]:
            with (
                self.subTest(mode=mode),
                tempfile.TemporaryDirectory(prefix="VoicePrompt-recovery-current-") as directory,
            ):
                before = json.loads(
                    checked(["dotnet", str(REFERENCE), "recovery-create", directory, mode])
                )
                path = Path(directory).joinpath(before["Id"], "manifest.enc")
                original_plain = checked(["dotnet", str(REFERENCE), "unprotect"], path.read_bytes())
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
                opened = checked(
                    [
                        str(PILOT),
                        "--recovery-open-journal-fixture",
                        directory,
                        before["Id"],
                    ]
                )
                self.assertEqual(opened, original_plain)
                self.assertFalse(any(path.parent.glob("*.tmp")))
                self.assertNotIn(b"synthetic", path.read_bytes())

    def test_old_v1_v2_manifests_preserve_unknown_cursors_and_context(self) -> None:
        audio = checked(["dotnet", str(REFERENCE), "wrap"], bytes([1]) * 6400)
        for version in [1, 2]:
            with (
                self.subTest(version=version),
                tempfile.TemporaryDirectory(prefix="VoicePrompt-recovery-legacy-") as directory,
            ):
                session = Path(directory).joinpath(ID)
                session.mkdir()
                path = session.joinpath("manifest.enc")
                path.write_bytes(checked([str(PILOT), "--protect-stdin"], legacy(version, audio)))
                session.joinpath("00000000.wav.enc").write_bytes(
                    checked([str(PILOT), "--protect-stdin"], audio)
                )
                before = json.loads(
                    checked(["dotnet", str(REFERENCE), "recovery-inspect", directory, ID])
                )
                self.assertFalse(before["Streaming"])
                self.assertEqual(before["Chunks"][0]["EndByte"], 0)
                self.assertEqual(before["Context"], "" if version == 1 else "synthetic context")
                latest = checked([str(PILOT), "--recovery-load-fixture", str(path)])
                self.assertEqual(struct.unpack_from("<i", latest, 4)[0], 3)
                checked([str(PILOT), "--recovery-save-fixture", str(path)], latest)
                after = json.loads(
                    checked(["dotnet", str(REFERENCE), "recovery-inspect", directory, ID])
                )
                self.assertEqual(before, after)
                opened = checked([str(PILOT), "--recovery-open-journal-fixture", directory, ID])
                self.assertEqual(opened, latest)

    def test_corrupt_plaintext_never_replaces_the_existing_encrypted_manifest(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-recovery-invalid-") as directory:
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
                refused = run([str(PILOT), "--recovery-save-fixture", str(path)], invalid)
                self.assertNotEqual(refused.returncode, 0)
                self.assertEqual(path.read_bytes(), before)
            path.write_bytes(b"corrupt synthetic ciphertext")
            refused = run([str(PILOT), "--recovery-load-fixture", str(path)])
            self.assertNotEqual(refused.returncode, 0)
            self.assertEqual(path.read_bytes(), b"corrupt synthetic ciphertext")

    def test_fixture_paths_cannot_escape_the_recovery_layout(self) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-recovery-paths-") as directory:
            for path in [
                Path(directory).joinpath("manifest.enc"),
                Path(directory).joinpath(ID.upper(), "manifest.enc"),
                Path(directory).joinpath(ID, "unexpected.bin"),
            ]:
                refused = run([str(PILOT), "--recovery-save-fixture", str(path)], b"synthetic")
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

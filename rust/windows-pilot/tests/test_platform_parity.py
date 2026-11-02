"""Compare native DPAPI with the actual C# protector using synthetic bytes only."""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
import unittest
import uuid
from pathlib import Path

PILOT: Path
REFERENCE: Path


def native(mode: str, data: bytes) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        [str(PILOT), mode], input=data, capture_output=True, timeout=20, check=False
    )


def reference(mode: str, data: bytes) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        ["dotnet", str(REFERENCE), mode],
        input=data,
        capture_output=True,
        timeout=20,
        check=False,
    )


class PlatformParityTests(unittest.TestCase):
    def test_installed_modes_reject_fixture_flags_before_personal_startup(self) -> None:
        with tempfile.TemporaryDirectory(
            prefix="VoicePrompt-installed-plan-"
        ) as directory:
            root = Path(directory)
            for arguments in [
                ["--installed", "--self-test", "--state-directory", str(root)],
                ["--self-test", "--state-directory", str(root), "--installed"],
                ["--self-test", "--installed"],
            ]:
                refused = subprocess.run(
                    [str(PILOT), *arguments],
                    capture_output=True,
                    timeout=20,
                    check=False,
                )
                self.assertNotEqual(refused.returncode, 0)
                self.assertIn(b"Installed mode cannot", refused.stderr)
                self.assertEqual(list(root.iterdir()), [])
            packaged = root.joinpath("VoicePrompt.exe")
            shutil.copyfile(PILOT, packaged)
            refused = subprocess.run(
                [str(packaged), "--self-test"],
                capture_output=True,
                timeout=20,
                check=False,
            )
            self.assertNotEqual(refused.returncode, 0)
            self.assertIn(b"Installed mode cannot", refused.stderr)
            self.assertEqual(list(root.iterdir()), [packaged])

    def test_single_instance_quit_is_bidirectionally_compatible_in_isolated_namespaces(
        self,
    ) -> None:
        for primary, helper in [
            (
                [str(PILOT), "--instance-hold-fixture"],
                ["dotnet", str(REFERENCE), "instance-quit-fixture"],
            ),
            (
                ["dotnet", str(REFERENCE), "instance-hold-fixture"],
                [str(PILOT), "--instance-quit-fixture"],
            ),
        ]:
            key = uuid.uuid4().hex
            process = subprocess.Popen(
                [*primary, key], stdout=subprocess.PIPE, stderr=subprocess.PIPE
            )
            try:
                assert process.stdout is not None
                with concurrent.futures.ThreadPoolExecutor(max_workers=1) as reader:
                    self.assertEqual(
                        reader.submit(process.stdout.readline)
                        .result(timeout=15)
                        .strip(),
                        b"ready",
                    )
                stopped = subprocess.run(
                    [*helper, key], capture_output=True, timeout=15, check=False
                )
                self.assertEqual(stopped.returncode, 0, stopped.stderr)
                _, error = process.communicate(timeout=15)
                self.assertEqual(process.returncode, 0, error)
            finally:
                if process.poll() is None:
                    process.kill()
                process.communicate(timeout=15)

    def test_dpapi_is_bidirectionally_compatible_with_current_csharp_entropy(
        self,
    ) -> None:
        synthetic_tokens = json.dumps(
            {
                "RefreshToken": "synthetic-not-a-real-refresh-token",
                "IdToken": "synthetic-not-a-real-id-token",
                "IdTokenExpiresAt": "2030-01-01T00:00:00+00:00",
                "Email": "fixture@example.invalid",
            }
        ).encode("utf-8")
        for data in [
            b"",
            b"\0\xff\x01",
            synthetic_tokens,
            random.Random(2026).randbytes(65536),
        ]:
            with self.subTest(size=len(data)):
                from_csharp = reference("protect", data)
                from_rust = native("--protect-stdin", data)
                self.assertEqual(from_csharp.returncode, 0, from_csharp.stderr)
                self.assertEqual(from_rust.returncode, 0, from_rust.stderr)
                to_rust = native("--unprotect-stdin", from_csharp.stdout)
                to_csharp = reference("unprotect", from_rust.stdout)
                self.assertEqual(to_rust.returncode, 0, to_rust.stderr)
                self.assertEqual(to_csharp.returncode, 0, to_csharp.stderr)
                self.assertEqual(to_rust.stdout, data)
                self.assertEqual(to_csharp.stdout, data)

    def test_corrupt_data_is_rejected_by_both_protectors(self) -> None:
        cipher = native("--protect-stdin", b"synthetic fixture")
        self.assertEqual(cipher.returncode, 0, cipher.stderr)
        tampered = bytearray(cipher.stdout)
        tampered[-1] ^= 0xFF
        for data in [b"", b"not a DPAPI envelope", cipher.stdout[:20], bytes(tampered)]:
            with self.subTest(size=len(data)):
                self.assertEqual(native("--unprotect-stdin", data).returncode, 2)
                self.assertEqual(reference("unprotect", data).returncode, 2)

    def test_invalid_startup_options_do_not_publish_readiness(self) -> None:
        with tempfile.TemporaryDirectory(
            prefix="VoicePrompt-native-options-"
        ) as directory:
            output = Path(directory).joinpath("ready.json")
            for extra in [
                ["--run-for-seconds", "0"],
                ["--run-for-seconds", "3601"],
                ["--self-test", "--instance-key", r"Local\VoicePrompt.SingleInstance"],
                ["--quit", "--ready-file", str(output)],
            ]:
                with self.subTest(arguments=extra):
                    result = subprocess.run(
                        [str(PILOT), *extra],
                        capture_output=True,
                        timeout=10,
                        check=False,
                    )
                    self.assertEqual(result.returncode, 2)
                    self.assertFalse(output.exists())

    def test_existing_readiness_is_not_overwritten(self) -> None:
        with tempfile.TemporaryDirectory(
            prefix="VoicePrompt-native-readiness-"
        ) as directory:
            output = Path(directory).joinpath("ready.json")
            output.write_text("sentinel", encoding="utf-8")
            result = subprocess.run(
                [
                    str(PILOT),
                    "--self-test",
                    "--instance-key",
                    uuid.uuid4().hex,
                    "--ready-file",
                    str(output),
                ],
                capture_output=True,
                timeout=10,
                check=False,
            )
            self.assertEqual(result.returncode, 2)
            self.assertEqual(output.read_text(encoding="utf-8"), "sentinel")
            self.assertEqual(list(Path(directory).iterdir()), [output])


if __name__ == "__main__":
    if os.name != "nt":
        raise SystemExit(
            "Native DPAPI parity requires Windows; not reported as a passing skip."
        )
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pilot", type=Path, required=True)
    parser.add_argument("--reference", type=Path, required=True)
    arguments = parser.parse_args()
    PILOT = arguments.pilot.resolve(strict=True)
    REFERENCE = arguments.reference.resolve(strict=True)
    unittest.main(argv=[sys.argv[0]], verbosity=2)

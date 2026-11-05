"""Compare native DPAPI with the actual C# protector using synthetic bytes only."""

from __future__ import annotations

import argparse
import concurrent.futures
import ctypes
import json
import os
import random
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import unittest
import uuid
from ctypes import wintypes
from pathlib import Path

PILOT: Path
REFERENCE: Path


def native(mode: str, data: bytes) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        [str(PILOT), mode], input=data, capture_output=True, timeout=20, check=False
    )


def reference(
    mode: str, data: bytes, *, timeout_seconds: int = 20
) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        ["dotnet", str(REFERENCE), mode],
        input=data,
        capture_output=True,
        timeout=timeout_seconds,
        check=False,
    )


class PlatformParityTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        started = time.perf_counter()
        prepared = reference(
            "protect",
            b"VoicePrompt synthetic CLR-DPAPI fixture preparation",
            timeout_seconds=60,
        )
        if prepared.returncode or not prepared.stdout:
            raise RuntimeError(
                f"C# CLR/DPAPI fixture preparation failed: {prepared.stderr!r}"
            )
        print(
            f"C# functional CLR/DPAPI preparation: "
            f"{(time.perf_counter() - started) * 1000:.3f}ms; bounded60s, operation budget20s",
            flush=True,
        )

    def test_release_executable_embeds_every_original_icon_image_byte_exact(
        self,
    ) -> None:
        original = (
            Path(__file__).resolve().parents[3]
            / "assets"
            / "windows"
            / "app.ico"
        ).read_bytes()
        self.assertEqual(original[:4], b"\0\0\1\0")
        count = struct.unpack_from("<H", original, 4)[0]
        self.assertGreater(count, 1)
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.LoadLibraryExW.argtypes = [
            wintypes.LPCWSTR,
            wintypes.HANDLE,
            wintypes.DWORD,
        ]
        kernel.LoadLibraryExW.restype = wintypes.HMODULE
        kernel.FreeLibrary.argtypes = [wintypes.HMODULE]
        kernel.FreeLibrary.restype = wintypes.BOOL
        kernel.FindResourceW.argtypes = [
            wintypes.HMODULE,
            ctypes.c_void_p,
            ctypes.c_void_p,
        ]
        kernel.FindResourceW.restype = wintypes.HANDLE
        kernel.SizeofResource.argtypes = [wintypes.HMODULE, wintypes.HANDLE]
        kernel.SizeofResource.restype = wintypes.DWORD
        kernel.LoadResource.argtypes = [wintypes.HMODULE, wintypes.HANDLE]
        kernel.LoadResource.restype = wintypes.HANDLE
        kernel.LockResource.argtypes = [wintypes.HANDLE]
        kernel.LockResource.restype = ctypes.c_void_p
        # Map resources only. Never execute this module or use installed app state.
        module = kernel.LoadLibraryExW(str(PILOT), None, 0x2 | 0x20)
        if not module:
            raise ctypes.WinError(ctypes.get_last_error())
        try:

            def resource(kind: int, identifier: int) -> bytes:
                entry = kernel.FindResourceW(module, identifier, kind)
                if not entry:
                    raise ctypes.WinError(ctypes.get_last_error())
                size = kernel.SizeofResource(module, entry)
                loaded = kernel.LoadResource(module, entry)
                address = kernel.LockResource(loaded) if loaded else None
                if not size or not address:
                    raise ctypes.WinError(ctypes.get_last_error())
                return ctypes.string_at(address, size)

            group = resource(14, 1)  # RT_GROUP_ICON, the product's first shell icon.
            self.assertEqual(group[:6], original[:6])
            self.assertEqual(len(group), 6 + count * 14)
            for index in range(count):
                source = original[6 + index * 16 : 6 + (index + 1) * 16]
                embedded = group[6 + index * 14 : 6 + (index + 1) * 14]
                self.assertEqual(embedded[:12], source[:12])
                length, offset = struct.unpack_from("<II", source, 8)
                identifier = struct.unpack_from("<H", embedded, 12)[0]
                self.assertEqual(
                    resource(3, identifier), original[offset : offset + length]
                )
        finally:
            if not kernel.FreeLibrary(module):
                raise ctypes.WinError(ctypes.get_last_error())

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

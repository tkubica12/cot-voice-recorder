"""Freeze verified synthetic reference contracts before removing the legacy apps.

Run only in the recorded pre-removal checkout. This is a migration exporter, not
an application, runtime fallback, performance profile or production-data reader.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
import zlib
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
CORE_MODES = {
    "wrap",
    "chunk",
    "cache",
    "pkce",
    "callback",
    "auth-url",
    "jwt",
    "refresh",
    "frame",
    "normalize-dictation",
    "normalize-batch",
    "stitch",
    "append-prefix",
    "legacy-state",
}
HTTP_HEADERS = (
    "Content-Type",
    "Cache-Control",
    "X-Request-Id",
    "Allow",
    "WWW-Authenticate",
)


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def source_hashes() -> dict[str, str]:
    files = [
        *ROOT.joinpath("windows", "src").rglob("*.cs"),
        *ROOT.joinpath("backend", "src").rglob("*.py"),
        ROOT / "rust" / "tests" / "reference" / "Program.cs",
    ]
    return {
        str(path.relative_to(ROOT)).replace("\\", "/"): digest(path.read_bytes())
        for path in sorted(files)
        if not {"obj", "bin", "__pycache__"}.intersection(path.parts)
    }


def load(name: str, relative: str):
    spec = importlib.util.spec_from_file_location(name, ROOT / relative)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Cannot load reference test {relative}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    if not output.is_dir() or any(output.iterdir()):
        raise ValueError("An existing empty output directory is required.")
    reference = ROOT / "rust/tests/reference/bin/Release/net8.0/ReferenceAudio.dll"
    examples = ROOT / "rust/target/release/examples"
    pilot = ROOT / "rust/target/release/VoicePromptRustPilot.exe"
    binaries = {
        "csharp": reference,
        "audio": examples / "audio_contract.exe",
        "client": examples / "client_contract.exe",
        "cloud": examples / "entity_contract.exe",
        "http": examples / "cloud_contract.exe",
        "native": pilot,
    }
    if any(not path.is_file() for path in binaries.values()):
        raise FileNotFoundError("Build the existing reference and contract probes first.")
    before = source_hashes()
    original_run = subprocess.run
    active = ""
    records: dict[str, dict] = {}
    http: list[dict] = []
    blobs: dict[str, bytes] = {}

    def blob(data: bytes) -> str:
        key = digest(data)
        blobs.setdefault(key, data)
        return key

    def capture(command, *positional, **kwargs):
        result = original_run(command, *positional, **kwargs)
        command = [str(part) for part in command]
        executable = Path(command[0]).name.lower()
        target = None
        modes: list[str] = []
        if executable in {"dotnet", "dotnet.exe"} and len(command) > 2:
            if Path(command[1]).resolve() == reference.resolve() and command[2] in CORE_MODES:
                target, modes = "csharp", command[2:]
        elif Path(command[0]).resolve() == binaries["cloud"].resolve():
            target, modes = "cloud", command[1:]
        elif Path(command[0]).resolve() == binaries["audio"].resolve():
            if command[1] in {"parse", "validate"}:
                target, modes = "audio", command[1:]
        elif (
            Path(command[0]).resolve() == binaries["client"].resolve()
            and command[1] == "summary-check"
        ):
            target, modes = "client", command[1:]
        if target is not None:
            data = kwargs.get("input", b"")
            if not isinstance(data, bytes):
                raise TypeError("Only explicit binary synthetic oracle inputs are supported.")
            key = json.dumps([target, modes, digest(data)], separators=(",", ":"))
            record = {
                "target": target,
                "args": modes,
                "input": blob(data),
                "stdout": blob(result.stdout),
                "returncode": result.returncode,
                "verified_by": [active],
            }
            if key in records:
                previous = records[key]
                if (previous["stdout"], previous["returncode"]) != (
                    record["stdout"],
                    record["returncode"],
                ):
                    raise AssertionError("A supposedly deterministic reference changed its result.")
                previous["verified_by"] = sorted(set(previous["verified_by"] + [active]))
            else:
                records[key] = record
        return result

    class Result(unittest.TextTestResult):
        def startTest(self, test):
            nonlocal active
            active = test.id()
            super().startTest(test)

    suite = unittest.TestSuite()
    modules = [
        ("audio", "rust/tests/test_audio_parity.py"),
        ("client", "rust/client-protocol/tests/test_client_parity.py"),
        ("dictation", "rust/client-protocol/tests/test_dictation_parity.py"),
        ("entity", "rust/cloud/tests/test_entity_parity.py"),
        ("edits", "rust/cloud/tests/test_edit_parity.py"),
        ("archive", "rust/cloud/tests/test_archive_parity.py"),
        ("relay", "rust/cloud/tests/test_relay_parity.py"),
        ("pipeline", "rust/cloud/tests/test_pipeline_text_parity.py"),
        ("http", "rust/cloud/tests/test_http_parity.py"),
    ]
    for name, relative in modules:
        module = load("legacy_export_" + name, relative)
        module.REFERENCE = reference
        module.DOTNET_ORACLE = reference
        module.RUST_ORACLE = binaries["audio"]
        module.CLIENT = binaries["client"]
        module.PILOT = pilot
        module.ORACLE = binaries["http"] if name == "http" else binaries["cloud"]
        suite.addTests(unittest.defaultTestLoader.loadTestsFromModule(module))

    from fastapi.testclient import TestClient

    request = TestClient.request

    def capture_http(client, method, url, **kwargs):
        response = request(client, method, url, **kwargs)
        headers = dict(kwargs.get("headers") or {})
        for name in list(headers):
            if name.lower() == "authorization":
                value = headers[name]
                headers[name] = "${VALID_BEARER}" if len(value) > 100 else "${INVALID_BEARER}"
        options = {"headers": headers}
        if kwargs.get("json") is not None:
            options["json"] = kwargs["json"]
        content = kwargs.get("content")
        if content is not None:
            if not isinstance(content, bytes):
                raise TypeError("Unexpected non-binary synthetic HTTP request content.")
            options["content_blob"] = blob(content)
        http.append(
            {
                "verified_by": active,
                "method": method,
                "path": str(url),
                "options": options,
                "status": response.status_code,
                "headers": {name: response.headers.get(name) for name in HTTP_HEADERS},
                "body": blob(response.content),
            }
        )
        return response

    with (
        patch.object(subprocess, "run", capture),
        patch.object(TestClient, "request", capture_http),
    ):
        result = unittest.TextTestRunner(verbosity=2, resultclass=Result).run(suite)
        if not result.wasSuccessful():
            raise RuntimeError("Reference comparison failed; no golden corpus was published.")
        active = "legacy_export_dictation.check_backend_model"
        sys.modules["legacy_export_dictation"].check_backend_model()
    if before != source_hashes():
        raise RuntimeError("Legacy source changed while its contracts were exported.")

    recovery = []
    for mode in ("legacy", "streaming", "audit"):
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-golden-recovery-") as directory:
            created = original_run(
                ["dotnet", str(reference), "recovery-create", directory, mode],
                input=b"",
                capture_output=True,
                timeout=60,
                check=True,
            )
            snapshot = json.loads(created.stdout)
            session = Path(directory) / snapshot["Id"]
            files = {}
            for path in sorted(session.iterdir()):
                if not path.is_file() or not path.name.endswith(".enc"):
                    raise ValueError("Unexpected C# synthetic recovery fixture entry.")
                plain = original_run(
                    ["dotnet", str(reference), "unprotect"],
                    input=path.read_bytes(),
                    capture_output=True,
                    timeout=60,
                    check=True,
                ).stdout
                files[path.name] = blob(plain)
            recovery.append({"mode": mode, "snapshot": snapshot, "plaintext_files": files})
    for key, data in blobs.items():
        output.joinpath(key + ".zlib").write_bytes(zlib.compress(data, level=9))
    manifest = {
        "schema": 1,
        "scope": "synthetic-reference-compatibility-not-production-or-performance",
        "source_commit": original_run(
            ["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, check=True
        )
        .stdout.decode()
        .strip(),
        "source_sha256": before,
        "binary_sha256": {name: digest(path.read_bytes()) for name, path in binaries.items()},
        "verification": {
            "legacy_comparison_tests": result.testsRun,
            "all_passed": True,
            "core_expectations": (
                "Actual C# stdout, independently compared with Rust by the original tests."
            ),
            "cloud_expectations": (
                "Rust stdout retained only after original real Python/SDK comparisons passed."
            ),
            "http_expectations": (
                "Actual FastAPI responses compared by the original signed Rust HTTP tests."
            ),
            "dpapi": (
                "No machine/user-bound ciphertext stored; only synthetic plaintext recovery wires."
            ),
        },
        "records": list(records.values()),
        "http": http,
        "recovery": recovery,
        "blobs": {key: {"bytes": len(data)} for key, data in sorted(blobs.items())},
    }
    output.joinpath("manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=True, indent=2) + "\n", encoding="utf-8"
    )
    print(
        json.dumps(
            {
                "tests": result.testsRun,
                "records": len(records),
                "http_requests": len(http),
                "recovery_modes": len(recovery),
                "unique_blobs": len(blobs),
                "corpus_bytes": sum(path.stat().st_size for path in output.iterdir()),
            }
        )
    )


if __name__ == "__main__":
    main()

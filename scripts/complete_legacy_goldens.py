"""Finish the synthetic pre-removal corpus using the still-present C# reference."""

from __future__ import annotations

import hashlib
import json
import subprocess
import tempfile
import unittest
import zlib
from datetime import UTC, datetime
from pathlib import Path

from export_legacy_contracts import ROOT, load, source_hashes


def main() -> None:
    output = ROOT / "rust/tests/golden"
    manifest = json.loads(output.joinpath("manifest.json").read_bytes())
    if manifest["source_sha256"] != source_hashes():
        raise RuntimeError("The reference sources differ from the original corpus.")
    if "windows_reference_tests" in manifest["verification"]:
        raise RuntimeError("The Windows supplement was already published.")
    reference = ROOT / "rust/tests/reference/bin/Release/net8.0/ReferenceAudio.dll"
    pilot = ROOT / "rust/target/release/VoicePromptRustPilot.exe"
    modules = [
        load("legacy_platform", "rust/windows-pilot/tests/test_platform_parity.py"),
        load("legacy_recovery", "rust/windows-pilot/tests/test_recovery_parity.py"),
    ]
    suite = unittest.TestSuite()
    for module in modules:
        module.REFERENCE, module.PILOT = reference, pilot
        suite.addTests(unittest.defaultTestLoader.loadTestsFromModule(module))
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    if not result.wasSuccessful():
        raise RuntimeError("Windows reference comparison failed; supplement not published.")

    def blob(data: bytes) -> str:
        key = hashlib.sha256(data).hexdigest()
        if key not in manifest["blobs"]:
            output.joinpath(key + ".zlib").write_bytes(zlib.compress(data, level=9))
            manifest["blobs"][key] = {"bytes": len(data)}
        return key

    def reference_call(args: list[str], data: bytes = b"") -> bytes:
        return subprocess.run(
            ["dotnet", str(reference), *args],
            input=data,
            capture_output=True,
            timeout=60,
            check=True,
        ).stdout

    audio = reference_call(["wrap"], bytes([1]) * 6400)
    for version in (1, 2):
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-old-wire-golden-") as directory:
            session = Path(directory) / modules[1].ID
            session.mkdir()
            plain = modules[1].legacy(version, audio)
            files = {"manifest.enc": plain, "00000000.wav.enc": audio}
            for name, data in files.items():
                session.joinpath(name).write_bytes(reference_call(["protect"], data))
            snapshot = json.loads(
                reference_call(
                    [
                        "recovery-inspect",
                        directory,
                        session.name,
                    ]
                )
            )
            manifest["recovery"].append(
                {
                    "mode": f"v{version}",
                    "snapshot": snapshot,
                    "plaintext_files": {name: blob(data) for name, data in files.items()},
                }
            )

    now = datetime.now(UTC).isoformat()
    settings = {
        "backend_base_url": "https://fixture.example.invalid",
        "notifications_paused": True,
        "auto_start": True,
        "dictation_enabled": False,
        "dictation_shortcut": "Ctrl+Alt+K",
        "dictation_toggle_shortcut": "Ctrl+Alt+Shift+K",
        "dictation_language": "cs",
        "dictation_refinement_enabled": True,
        "dictation_streaming_enabled": False,
        "dictation_final_wait_milliseconds": 3500,
    }
    history = [
        {
            "transcript_id": "fixture",
            "recording_id": "r",
            "preview": "synthetic",
            "body": "synthetic body",
            "raw_body": "synthetic original",
            "completed_at": now,
            "cached_at": now,
            "character_count": 14,
        }
    ]
    cache = {
        "RefreshToken": "synthetic-rotated-refresh",
        "IdToken": "synthetic-id",
        "IdTokenExpiresAt": "2030-01-01T10:30:00+00:00",
        "Email": "fixture@example.invalid",
    }
    manifest["persistence"] = {}
    with tempfile.TemporaryDirectory(prefix="VoicePrompt-wire-files-golden-") as directory:
        for kind, mode, value in (
            ("settings", "settings-save", settings),
            ("history", "history-add", history),
            ("cache", "cache-save", cache),
        ):
            path = Path(directory) / kind
            encoded = json.dumps(value, ensure_ascii=False).encode()
            reference_call([mode, str(path)], encoded)
            wire = path.read_bytes()
            if kind == "cache":
                wire = reference_call(["unprotect"], wire)
            manifest["persistence"][kind] = {"input": blob(encoded), "wire": blob(wire)}

    import sys

    sys.path.insert(0, str(ROOT / "backend/src"))
    from voice_recorder.ai.dictation_refinement import SYSTEM_PROMPT
    from voice_recorder.problems import ValidationProblemError
    from voice_recorder.prompts import REFINEMENT_SYSTEM_PROMPT
    from voice_recorder.wav import parse_wav, validate_wav

    for record in manifest["records"]:
        if record["target"] != "audio" or record["returncode"] == 0:
            continue
        data = zlib.decompress(output.joinpath(record["input"] + ".zlib").read_bytes())
        mode, strict = record["args"]
        try:
            if mode == "validate":
                validate_wav(
                    data,
                    strict=strict == "strict",
                    sample_rate=16000,
                    channels=1,
                    bits_per_sample=16,
                )
            else:
                parse_wav(data, strict=strict == "strict")
        except ValidationProblemError as error:
            record["required_diagnostics"] = [
                item["message"] if item["field"] == "body" else item["field"] + ":"
                for item in error.errors or []
            ]
        else:
            raise AssertionError("A frozen negative audio case is no longer negative.")
    manifest["prompts"] = {
        "dictation-system.txt": blob(SYSTEM_PROMPT.encode()),
        "recording-system.txt": blob(REFINEMENT_SYSTEM_PROMPT.encode()),
    }
    if manifest["source_sha256"] != source_hashes():
        raise RuntimeError("Reference sources changed during the Windows supplement.")
    manifest["verification"]["windows_reference_tests"] = result.testsRun
    manifest["verification"]["all_windows_passed"] = True
    manifest["exporter_sha256"] = {
        name: hashlib.sha256(ROOT.joinpath("scripts", name).read_bytes()).hexdigest()
        for name in ("export_legacy_contracts.py", "complete_legacy_goldens.py")
    }
    output.joinpath("manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=True, indent=2) + "\n", encoding="utf-8"
    )
    print(
        json.dumps(
            {
                "windows_reference_tests": result.testsRun,
                "recovery_cases": len(manifest["recovery"]),
                "persistence_cases": len(manifest["persistence"]),
                "prompts": len(manifest["prompts"]),
            }
        )
    )


if __name__ == "__main__":
    main()

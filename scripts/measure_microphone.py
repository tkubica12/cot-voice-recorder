"""Separately consented 30+30 count-only physical readiness profile; never run by CI."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import statistics
import subprocess
import time
from pathlib import Path

from measure_clients import p95
from measure_synthetic_capture import create

SCOPE = "manual-default-microphone-240ms-count-only"
TEST = "microphone::tests::manual_default_device_240ms_count_only_no_store"
APPROVAL = "one-batch-30-rust-30-csharp-240ms-count-only"


def reading(stdout: bytes) -> dict:
    candidates = []
    for line in stdout.decode("utf-8").splitlines():
        if "{" in line:
            candidate = json.loads(line[line.index("{") :])
            if candidate.get("scope") == SCOPE:
                candidates.append(candidate)
    if len(candidates) != 1:
        raise ValueError("Exactly one typed count-only physical reading is required.")
    result = candidates[0]
    for flag in [
        "saved_or_uploaded",
        "clipboard_or_keyboard",
        "full_migration_acceptance",
    ]:
        if result.get(flag) is not False:
            raise ValueError("Probe did not attest the approved no-store scope.")
    for field in ["open_ms", "first_pcm_ms", "stop_ms"]:
        value = result.get(field)
        if type(value) not in {int, float} or not math.isfinite(value) or value <= 0:
            raise ValueError(f"Missing actual finite {field}.")
    count = result.get("pcm_bytes_counted_then_zeroed")
    if type(count) is not int or count < 1280 or count % 2:
        raise ValueError("No complete PCM buffer was observed.")
    if type(result.get("buffers")) is not int or result["buffers"] <= 0:
        raise ValueError("No actual buffer count was observed.")
    return result


def summary(rows: list[dict]) -> tuple[dict, bool]:
    result = {}
    if len(rows) != 60:
        raise ValueError(
            "The complete approved batch requires exactly 60 observations."
        )
    for implementation in ["csharp", "rust"]:
        selected = [row for row in rows if row["implementation"] == implementation]
        if sorted(row["trial"] for row in selected) != list(range(1, 31)):
            raise ValueError("Every implementation needs all 30 distinct trials.")
        values = [reading(json.dumps(row["reading"]).encode()) for row in selected]
        result[implementation] = {
            field: {
                "p95": p95([value[field] for value in values]),
                "mean": statistics.mean(value[field] for value in values),
                "max": max(value[field] for value in values),
            }
            for field in ["open_ms", "first_pcm_ms", "stop_ms"]
        }
    passes = (
        result["rust"]["first_pcm_ms"]["p95"]
        <= 1.10 * result["csharp"]["first_pcm_ms"]["p95"]
    )
    return result, passes


def bundle(binary: Path, implementation: str) -> dict:
    names = (
        ["VoicePromptManualMicFixture.exe"]
        if implementation == "rust"
        else [
            "DictationProbe.exe",
            "DictationProbe.dll",
            "DictationProbe.deps.json",
            "DictationProbe.runtimeconfig.json",
            "VoicePrompt.dll",
            "VoicePrompt.Core.dll",
            "VoicePrompt.deps.json",
            "VoicePrompt.runtimeconfig.json",
            "NAudio.Core.dll",
            "NAudio.WinMM.dll",
        ]
    )
    if binary.name != names[0]:
        raise ValueError("Only explicitly named dedicated probes may be launched.")
    return {
        name: hashlib.sha256(binary.parent.joinpath(name).read_bytes()).hexdigest()
        for name in names
    }


def run(csharp: Path, rust: Path, output: Path, revision: str, approval: str) -> None:
    if os.name != "nt" or approval != APPROVAL:
        raise ValueError(
            "The separate exact 30+30 physical batch approval is required."
        )
    # This opt-in records scope; it never substitutes for the user's permission.
    output = output.resolve(strict=True)
    if any(output.iterdir()):
        raise FileExistsError("A new empty evidence directory is required.")
    binaries = {"csharp": csharp, "rust": rust}
    hashes = {name: bundle(binary, name) for name, binary in binaries.items()}
    personal = subprocess.run(
        [
            "pwsh",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            (
                "$p=@(Get-Process -Name VoicePrompt,VoicePromptManualMicFixture,DictationProbe -ErrorAction SilentlyContinue);"
                " if ($p.Count) { Write-Error 'A relevant personal/physical client is running.'; exit 1 }; exit 0"
            ),
        ],
        capture_output=True,
        timeout=30,
        check=False,
        creationflags=subprocess.CREATE_NO_WINDOW,
    )
    if personal.returncode:
        raise RuntimeError(
            "Personal/physical client preflight failed; no microphone was opened."
        )
    environment = {
        key: value
        for key, value in os.environ.items()
        if not key.upper().startswith(
            ("VOICEPROMPT_", "AZURE_", "IDENTITY_", "GOOGLE_", "VR_")
        )
    }
    environment["VOICEPROMPT_MANUAL_MICROPHONE_PROBE"] = "240ms-count-only-no-store"
    environment["DOTNET_DISABLE_GUI_ERRORS"] = "1"
    rows = []
    try:
        for trial in range(1, 31):
            for name in ["csharp", "rust"] if trial % 2 else ["rust", "csharp"]:
                arguments = (
                    ["--microphone"]
                    if name == "csharp"
                    else [
                        TEST,
                        "--exact",
                        "--ignored",
                        "--nocapture",
                        "--test-threads=1",
                    ]
                )
                started = time.time()
                create(
                    output.joinpath(f"{trial:02}-{name}-started.json"),
                    json.dumps(
                        {
                            "trial": trial,
                            "implementation": name,
                            "started_unix_seconds": started,
                            "requested_capture_ms": 240,
                            "automatic_retry": False,
                        }
                    ).encode(),
                )
                child = subprocess.Popen(
                    [str(binaries[name]), *arguments],
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    env=environment,
                    creationflags=subprocess.CREATE_NO_WINDOW,
                )
                try:
                    stdout, stderr = child.communicate(timeout=15)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.communicate(timeout=10)
                    raise
                if child.returncode or stderr:
                    raise RuntimeError(
                        f"{name} trial {trial} failed; no retry: "
                        f"stdout={stdout!r}; stderr={stderr!r}"
                    )
                if name == "rust" and (
                    b"1 passed" not in stdout or b"0 ignored" not in stdout
                ):
                    raise RuntimeError(
                        "The exact physical native test did not execute."
                    )
                row = {
                    "trial": trial,
                    "implementation": name,
                    "reading": reading(stdout),
                    "started_unix_seconds": started,
                    "pid": child.pid,
                    "exit_code": child.returncode,
                    "process_exited": True,
                }
                create(
                    output.joinpath(f"{trial:02}-{name}-result.json"),
                    json.dumps(row).encode(),
                )
                rows.append(row)
        if hashes != {name: bundle(binary, name) for name, binary in binaries.items()}:
            raise RuntimeError("A physical probe bundle changed during the batch.")
        statistics_by_implementation, passes = summary(rows)
        result = {
            "status": "complete",
            "scope": "physical-default-device-paired-first-pcm-readiness",
            "source_revision": revision,
            "approved_batch": APPROVAL,
            "requested_capture_ms_per_trial": 240,
            "requested_capture_windows_total_ms": 14400,
            "trial_count_per_implementation": 30,
            "automatic_retries": 0,
            "binary_bundle_sha256": hashes,
            "statistics_ms": statistics_by_implementation,
            "criterion": "native p95 first-valid-PCM <=110% C# baseline",
            "readiness_threshold_passed": passes,
            "whole_migration_acceptance": False,
            "saved_or_uploaded_audio": False,
            "clipboard_keyboard_or_auth": False,
            "rows": rows,
        }
        create(
            output.joinpath("microphone-readiness.json"),
            json.dumps(result, indent=2).encode(),
        )
        print(
            json.dumps({key: value for key, value in result.items() if key != "rows"}),
            flush=True,
        )
    except BaseException as error:
        create(
            output.joinpath("failure.json"),
            json.dumps(
                {
                    "status": "failed",
                    "error": str(error),
                    "completed_observation_count": len(rows),
                    "automatic_retry": False,
                    "whole_migration_acceptance": False,
                },
                indent=2,
            ).encode(),
        )
        raise


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--csharp", type=Path, required=True)
    parser.add_argument("--rust", type=Path, required=True)
    parser.add_argument("--output-directory", type=Path, required=True)
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--approved-batch", choices=[APPROVAL], required=True)
    args = parser.parse_args()
    run(
        args.csharp.resolve(strict=True),
        args.rust.resolve(strict=True),
        args.output_directory,
        args.source_revision,
        args.approved_batch,
    )

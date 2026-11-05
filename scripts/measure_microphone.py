"""Separately consented 30+30 count-only physical readiness profile; never run by CI."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import statistics
from pathlib import Path

from measure_clients import p95

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
        raise ValueError("The complete approved batch requires exactly 60 observations.")
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
    passes = result["rust"]["first_pcm_ms"]["p95"] <= 1.10 * result["csharp"]["first_pcm_ms"]["p95"]
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
    raise RuntimeError(
        "The approved paired microphone measurement is complete and its C# "
        "reference application was retired. No capture is started by this tool."
    )


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

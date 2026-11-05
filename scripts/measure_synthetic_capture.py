"""Owned engine or current-controller synthetic PCM longevity, never physical/GUI acceptance."""

from __future__ import annotations

import argparse
import errno
import hashlib
import json
import os
import statistics
import subprocess
import tempfile
import threading
import time
from collections import deque
from pathlib import Path
from typing import BinaryIO
from synthetic_fixture_root import FixtureRoot, guard

ROOT = Path(__file__).resolve().parents[1]
SCOPE = "native-streaming-engine-realtime-synthetic-pcm"
TEST = "streaming_session::longevity_tests::realtime_synthetic_hour"
CONTROLLER_SCOPE = "native-controller-realtime-synthetic-pcm"
CONTROLLER_TEST = "dictation::controller_tests::realtime_synthetic_controller_hour"


def create(path: Path, data: bytes) -> None:
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as output:
        temporary = Path(output.name)
        try:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        except BaseException:
            output.close()
            temporary.unlink()
            raise
    try:
        os.link(temporary, path)
    finally:
        temporary.unlink()


def wait(path: Path, process: subprocess.Popen, seconds: int) -> dict:
    began = time.monotonic()
    while not path.exists():
        if process.poll() is not None:
            raise RuntimeError(
                f"Synthetic engine exited before {path.name}: {process.returncode}"
            )
        if time.monotonic() - began > seconds:
            raise TimeoutError(f"Synthetic engine did not publish {path.name}.")
        time.sleep(0.01)
    return json.loads(path.read_bytes())


class ReadinessError(RuntimeError):
    def __init__(self, report: dict):
        self.report = report
        super().__init__("Owned ready control failed: " + json.dumps(report))


def wait_ready(
    owned: FixtureRoot, process: subprocess.Popen, binary: Path, digest: str, seconds: float
) -> dict:
    path = owned.path / "ready.json"
    expected = {
        "scope": CONTROLLER_SCOPE, "pid": process.pid, "physical_ports": False,
        "network_or_auth": False, "full_client_profile": False,
        "current_controller": True, "renderer": False, "main_ui_or_tray": False,
        "real_auth_or_pubsub": False, "polisher_and_outbox": True,
    }
    started = time.monotonic()
    deadline = started + seconds
    attempts = 0
    first = last = None

    def fail(reason: str) -> None:
        raise ReadinessError({
            "control_file": path.name, "reason": reason, "attempts": attempts,
            "elapsed_seconds": time.monotonic() - started,
            "deadline_seconds": seconds, "native_pid": process.pid,
            "native_exit_code": process.poll(), "first_error": first, "last_error": last,
        })

    while True:
        owned.owned()
        if process.poll() is not None:
            fail("owned native exited before validated readiness")
        if time.monotonic() >= deadline:
            fail("original ready deadline expired")
        attempts += 1
        try:
            guard(path)
            with path.open("rb") as source:
                data = source.read(4097)
            if len(data) > 4096:
                fail("ready control exceeds 4096 bytes")
        except OSError as error:
            item = {
                "type": type(error).__name__, "errno": error.errno,
                "winerror": getattr(error, "winerror", None),
                "message": str(error.strerror or type(error).__name__)[:256],
            }
            if first is None:
                first = item
            last = item
            winerror = getattr(error, "winerror", None)
            if not (
                winerror in (2, 3, 5, 32, 33)
                or (winerror is None and error.errno in (errno.ENOENT, errno.EACCES))
            ):
                fail("non-transient control read error")
            time.sleep(min(0.01, max(0, deadline - time.monotonic())))
            continue
        try:
            ready = json.loads(data)
        except (json.JSONDecodeError, UnicodeDecodeError) as error:
            fail(f"invalid complete ready JSON: {type(error).__name__}")
        if (
            type(ready) is not dict or ready.keys() != expected.keys()
            or any(type(ready[key]) is not type(value) or ready[key] != value
                   for key, value in expected.items())
        ):
            fail("ready scope/PID/fields do not match the owned native")
        owned.owned()
        if hashlib.sha256(binary.read_bytes()).hexdigest() != digest:
            fail("owned native binary hash changed")
        if process.poll() is not None or time.monotonic() >= deadline:
            fail("native exit or deadline during ready validation")
        create(owned.evidence / "readiness-read.json", json.dumps({
            "status": "validated", "attempts": attempts,
            "elapsed_seconds": time.monotonic() - started,
            "deadline_seconds": seconds, "first_error": first, "last_error": last,
            "native_pid": process.pid, "binary_sha256": digest,
            "root_marker_and_directory_identity_verified": True,
        }).encode())
        return ready


def start_readers(process: subprocess.Popen, readers: list[threading.Thread]) -> None:
    started = []
    try:
        for reader in readers:
            reader.start()
            started.append(reader)
    except BaseException as error:
        if process.poll() is None:
            process.kill()
        process.wait(timeout=10)
        for reader in started:
            reader.join(timeout=5)
        process.stdout.close()
        process.stderr.close()
        error.add_note(f"Native log setup failed; owned PID {process.pid} exited, data root retained.")
        raise


def sample(
    process: subprocess.Popen,
    binary_hash: str,
    output: Path,
    count: int,
    revision: str,
    scope: str = SCOPE,
) -> dict:
    subprocess.run(
        [
            "pwsh",
            "-NoProfile",
            "-NonInteractive",
            "-File",
            str(ROOT.joinpath("scripts", "Measure-VoicePromptProcess.ps1")),
            "-ProcessId",
            str(process.pid),
            "-Scenario",
            scope,
            "-SampleCount",
            str(count),
            "-IntervalMilliseconds",
            "1000",
            "-SourceRevision",
            revision,
            "-OutputPath",
            str(output),
            *(
                ["-SampleJournalPath", str(output.with_suffix(".jsonl"))]
                if scope == CONTROLLER_SCOPE
                else []
            ),
        ],
        check=True,
        timeout=count + 60,
    )
    report = json.loads(output.read_text(encoding="utf-8-sig"))
    if (
        report["status"] != "complete"
        or report["scenario"] != scope
        or report["source_revision"] != revision
        or report["process"]["id"] != process.pid
        or report["process"]["binary_sha256"].lower() != binary_hash
        or report["measurement"]["sample_count"] != count
        or report["measurement"]["requested_interval_milliseconds"] != 1000
        or len(report["samples"]) != count
    ):
        raise RuntimeError("Owned-process sampler identity or complete shape mismatch.")
    if process.poll() is not None:
        raise RuntimeError("Owned engine exited during sampling.")
    if scope == CONTROLLER_SCOPE:
        rows = [
            json.loads(line)
            for line in output.with_suffix(".jsonl").read_text().splitlines()
        ]
        header = rows[0]
        if (
            len(rows) != count + 1
            or header["process_id"] != process.pid
            or header["scenario"] != scope
            or header["source_revision"] != revision
            or header["binary_sha256"].lower() != binary_hash
            or header["requested_samples"] != count
            or header["interval_milliseconds"] != 1000
            or any(
                row["index"] != index or row["sample"] != report["samples"][index]
                for index, row in enumerate(rows[1:])
            )
        ):
            raise RuntimeError(
                "Durable raw sample journal does not match the completed sampler."
            )
    return report


def run(
    binary: Path,
    directory: Path,
    revision: str,
    audio_seconds: int,
    idle_samples: int,
    controller: bool = False,
) -> None:
    scope = CONTROLLER_SCOPE if controller else SCOPE
    test = CONTROLLER_TEST if controller else TEST
    expected_binary = (
        "VoicePromptNativeControllerFixture.exe"
        if controller
        else "VoicePromptNativeCaptureFixture.exe"
    )
    if os.name != "nt" or binary.name != expected_binary:
        raise ValueError(
            "An explicitly named frozen Windows libtest fixture is required."
        )
    if not 2 <= audio_seconds <= 3600 or not 2 <= idle_samples <= 301:
        raise ValueError(
            "Synthetic PCM and idle sampling must use bounded explicit counts."
        )
    directory = directory.resolve(strict=True)
    if any(directory.iterdir()):
        raise ValueError("A new empty evidence directory is required.")
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    with FixtureRoot(directory) as owned:
        control = owned.path
        nonce = owned.nonce
        create(
            control.joinpath("capture-options.json"),
            json.dumps(
                {
                    "scope": scope,
                    "nonce": nonce,
                    "audio_seconds": audio_seconds,
                }
            ).encode(),
        )
        environment = dict(os.environ)
        environment["VOICEPROMPT_SYNTHETIC_HOUR_DIRECTORY"] = str(control)
        process = subprocess.Popen(
            [
                str(binary),
                test,
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ],
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        stdout: deque[bytes] = deque(maxlen=200)
        stderr: deque[bytes] = deque(maxlen=200)

        def drain(stream: BinaryIO, tail: deque[bytes]) -> None:
            for line in stream:
                tail.append(line[-16384:])

        readers = [
            threading.Thread(target=drain, args=(process.stdout, stdout), daemon=True),
            threading.Thread(target=drain, args=(process.stderr, stderr), daemon=True),
        ]
        start_readers(process, readers)

        def logs() -> tuple[bytes, bytes]:
            for reader in readers:
                reader.join(timeout=5)
            return b"".join(stdout), b"".join(stderr)

        try:
            ready = wait(control.joinpath("ready.json"), process, 15)
            expected_ready = {
                "scope": scope,
                "pid": process.pid,
                "physical_ports": False,
                "network_or_auth": False,
                "full_client_profile": False,
            }
            if controller:
                expected_ready.update(
                    {
                        "current_controller": True,
                        "renderer": False,
                        "main_ui_or_tray": False,
                        "real_auth_or_pubsub": False,
                        "polisher_and_outbox": True,
                    }
                )
            if ready != expected_ready:
                raise RuntimeError(
                    "Engine fixture readiness did not prove the expected scope."
                )
            print(
                f"Sampling warm {'controller' if controller else 'engine'} idle, owned PID {process.pid}",
                flush=True,
            )
            before = sample(
                process,
                digest,
                directory.joinpath("before.json"),
                idle_samples,
                revision,
                scope,
            )
            create(control.joinpath("begin"), nonce.encode())
            print(
                f"Sampling actual {audio_seconds}s synthetic capture; no physical input",
                flush=True,
            )
            during = sample(
                process,
                digest,
                directory.joinpath("capture.json"),
                audio_seconds + 1,
                revision,
                scope,
            )
            completed = wait(control.joinpath("idle-ready.json"), process, 30)
            if (
                completed["audio_seconds"] != audio_seconds
                or completed["captured_bytes"] != audio_seconds * 32000
                or completed["confirmed_bytes"] != audio_seconds * 32000
                or completed["verified_provider_bytes"] != audio_seconds * 32000
                or completed["actual_capture_seconds"] < audio_seconds - 0.1
                or not completed["stream_starts"]
                or completed["stream_starts"][0] != 0
                or (audio_seconds > 3000 and len(completed["stream_starts"]) < 2)
            ):
                raise RuntimeError(
                    "Exact PCM, durable completion or real-time duration was not proved."
                )
            if controller and (
                not completed.get("current_controller")
                or completed.get("renderer") is not False
                or not completed.get("history_saved_before_handoff")
                or not completed.get("capture_joined")
                or not completed.get("polisher_exercised")
                or not completed.get("outbox_acknowledged")
                or not completed.get("confirmed_cursors")
                or completed["confirmed_cursors"][-1] != audio_seconds * 32000
                or any(
                    left >= right
                    for left, right in zip(
                        completed["confirmed_cursors"],
                        completed["confirmed_cursors"][1:],
                    )
                )
            ):
                raise RuntimeError(
                    "Current controller, outbox or ordered confirmation proof is incomplete."
                )
            print(
                "Sampling the same current-controller idle after capture join/outbox acknowledgment"
                if controller
                else "Sampling the same warmed empty-recovery idle after joined engine shutdown",
                flush=True,
            )
            after = sample(
                process,
                digest,
                directory.joinpath("after.json"),
                idle_samples,
                revision,
                scope,
            )
            create(control.joinpath("finish"), nonce.encode())
            process.wait(timeout=15)
            out, error = logs()
            if process.returncode or error:
                raise RuntimeError(
                    f"Synthetic engine failed: {process.returncode}; {out!r} {error!r}"
                )
            if b"1 passed" not in out or b"0 ignored" not in out:
                raise RuntimeError(
                    "The exact ignored fixture test did not actually execute."
                )
            if hashlib.sha256(binary.read_bytes()).hexdigest() != digest:
                raise RuntimeError(
                    "Frozen native fixture changed during the measurement."
                )
            if controller:
                exit_proof = json.loads(
                    control.joinpath("normal-exit.json").read_bytes()
                )
                if (
                    not exit_proof.get("current_controller")
                    or exit_proof.get("physical_ports") is not False
                ):
                    raise RuntimeError(
                        "Current controller did not finish its normal owned shutdown."
                    )
            for name, data in (("stdout.txt", out), ("stderr.txt", error)):
                create(directory.joinpath(name), data)
            progress = {
                path.name: json.loads(path.read_bytes())
                for path in control.glob("*.json")
                if path.name != "capture-options.json"
                and path.name != "migration-fixture.json"
            }
            create(
                directory.joinpath("progress-evidence.json"),
                json.dumps(progress, indent=2).encode(),
            )
            baseline = statistics.mean(
                row["private_bytes"] for row in before["samples"]
            )
            final = statistics.mean(row["private_bytes"] for row in after["samples"])
            growth = final - baseline
            maximum_growth = max(
                row["private_bytes"] for row in after["samples"]
            ) - statistics.median(row["private_bytes"] for row in before["samples"])
            result = {
                "status": "complete",
                "schema_version": 1,
                "scope": scope,
                "source_revision": revision,
                "binary_sha256": digest,
                "pid": process.pid,
                "full_client_profile": False,
                "whole_migration_acceptance": False,
                "physical_ports_or_user_data": False,
                "network_or_auth": False,
                "current_controller": controller,
                "renderer": False,
                "audio_seconds": audio_seconds,
                "idle_sample_count": idle_samples,
                "actual_capture": completed,
                "before_mean_private_bytes": baseline,
                "after_mean_private_bytes": final,
                "private_growth_bytes": growth,
                "maximum_idle_growth_bytes": maximum_growth,
                "sufficient_for_synthetic_engine_hour": audio_seconds == 3600
                and idle_samples == 301
                and not controller,
                "sufficient_for_synthetic_controller_hour": audio_seconds == 3600
                and idle_samples == 301
                and controller,
                "observed_growth_at_most_10_mib": maximum_growth <= 10 * 1024 * 1024,
                "peak_capture_private_bytes": max(
                    row["private_bytes"] for row in during["samples"]
                ),
                "raw_reports": ["before.json", "capture.json", "after.json"],
                "durable_sample_journals": (
                    ["before.jsonl", "capture.jsonl", "after.jsonl"]
                    if controller
                    else []
                ),
                "exclusions": [
                    "native GUI/renderer/tray" if controller else "full GUI/controller",
                    "microphone",
                    "clipboard/paste",
                    "auth",
                    "actual cloud/model transport",
                    "signed-in client acceptance",
                ],
            }
            owned.preserve(process, failed=False)
        except BaseException as error:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=10)
            out, failure = logs()
            progress = {
                path.name: json.loads(path.read_bytes())
                for path in control.glob("*.json")
                if path.name
                in {
                    "ready.json",
                    "producer-complete.json",
                    "producer-failure.json",
                    "capture-sealed.json",
                    "transcription-complete.json",
                    "shutdown-complete.json",
                    "idle-ready.json",
                }
                or path.name.startswith("progress-")
            }
            owned.preserve(process, failed=True)
            create(
                directory.joinpath("failure.json"),
                json.dumps(
                    {
                        "status": "failed",
                        "scope": scope,
                        "source_revision": revision,
                        "pid": process.pid,
                        "binary_sha256": digest,
                        "error": str(error),
                        "stdout": out.decode(errors="replace"),
                        "stderr": failure.decode(errors="replace"),
                        "progress": progress,
                        "whole_migration_acceptance": False,
                    },
                    indent=2,
                ).encode(),
            )
            error.add_note(
                f"Native fixture stderr: {failure.decode(errors='replace')[-4096:]}"
            )
            raise
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=10)
            logs()
            process.stdout.close()
            process.stderr.close()
    if control.exists():
        raise RuntimeError("Owned synthetic controller directory was not removed.")
    result["owned_control_directory_removed"] = True
    result["acceptance_status"] = (
        "incomplete"
        if audio_seconds != 3600 or idle_samples != 301
        else "pass"
        if result["observed_growth_at_most_10_mib"]
        else "fail"
    )
    create(
        directory.joinpath(
            "synthetic-controller-hour.json"
            if controller
            else "synthetic-engine-hour.json"
        ),
        json.dumps(result, indent=2).encode(),
    )
    print(
        json.dumps(
            {
                key: value
                for key, value in result.items()
                if key not in {"actual_capture", "exclusions"}
            }
        ),
        flush=True,
    )


def run_functional(
    binary: Path, directory: Path, revision: str, audio_seconds: int,
    native_revision: str | None = None,
) -> None:
    """Current controller only: no debugger or process/resource/idle sampler."""
    if os.name != "nt" or binary.name != "VoicePromptNativeControllerFixture.exe":
        raise ValueError("An explicitly named frozen Windows controller libtest is required.")
    if not 2 <= audio_seconds <= 3600:
        raise ValueError("Functional capture must use bounded explicit seconds.")
    directory = directory.resolve(strict=True)
    if any(directory.iterdir()):
        raise ValueError("A new empty evidence directory is required.")
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    result = {}
    with FixtureRoot(directory) as owned:
        control, nonce = owned.path, owned.nonce
        create(control / "capture-options.json", json.dumps({
            "scope": CONTROLLER_SCOPE, "nonce": nonce, "audio_seconds": audio_seconds,
        }).encode())
        environment = dict(os.environ)
        environment["VOICEPROMPT_SYNTHETIC_HOUR_DIRECTORY"] = str(control)
        process = subprocess.Popen(
            [str(binary), CONTROLLER_TEST, "--exact", "--ignored", "--nocapture", "--test-threads=1"],
            env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        log_errors: list[str] = []

        def drain(stream: BinaryIO, path: Path) -> None:
            total = 0
            output = None
            try:
                output = path.open("xb")
                for line in stream:
                    total += len(line)
                    if total > 16 * 1024 * 1024:
                        raise OSError("Bounded native functional log exceeds 16MiB.")
                    output.write(line)
                    output.flush()
                    os.fsync(output.fileno())
            except OSError as error:
                log_errors.append(f"{path.name}: {error}")
                # Keep the pipe draining so a diagnostic-write failure cannot deadlock the child.
                for _ in stream:
                    pass
            finally:
                if output is not None:
                    output.close()

        readers = [
            threading.Thread(target=drain, args=(process.stdout, directory / "stdout.txt"), daemon=True),
            threading.Thread(target=drain, args=(process.stderr, directory / "stderr.txt"), daemon=True),
        ]
        start_readers(process, readers)

        def joined_logs() -> tuple[str, str]:
            for reader in readers:
                reader.join(timeout=5)
                if reader.is_alive():
                    raise RuntimeError("Owned native functional log reader did not join.")
            if log_errors:
                raise RuntimeError("Native log preservation failed: " + "; ".join(log_errors))
            return tuple((directory / name).read_text(errors="replace") for name in ["stdout.txt", "stderr.txt"])

        try:
            create(directory / "native-owner.json", json.dumps({
                "collector_pid": os.getpid(), "native_pid": process.pid,
                "binary_path": str(binary), "binary_sha256": digest,
                "source_revision": revision, "native_source_revision": native_revision or revision,
                "control_directory": str(control), "scope": CONTROLLER_SCOPE,
                "functional_only": True, "resource_sampling": False,
            }).encode())
            ready = wait_ready(owned, process, binary, digest, 15)
            create(directory / "readiness.json", json.dumps(ready).encode())
            owned.owned()
            if process.poll() is not None:
                raise RuntimeError("Owned native exited before main begin.")
            create(control / "begin", nonce.encode())
            deadline = time.monotonic() + audio_seconds + 60
            minute = 60
            with (directory / "functional-progress.jsonl").open("xb") as progress:
                while not (control / "idle-ready.json").exists():
                    if process.poll() is not None:
                        raise RuntimeError(f"Native functional controller exited: {process.returncode}")
                    if time.monotonic() > deadline:
                        raise TimeoutError("Functional capture/completion wall budget expired.")
                    point = control / f"progress-{minute:04}.json"
                    if point.exists():
                        item = json.loads(point.read_bytes())
                        row = {"minute": minute // 60, **{
                            key: item[key] for key in ["elapsed_seconds", "accepted_bytes",
                            "writer_received_bytes", "writer_completed_bytes", "saved_bytes", "confirmed_bytes"]
                        }}
                        progress.write(json.dumps(row).encode() + b"\n")
                        progress.flush()
                        os.fsync(progress.fileno())
                        print(json.dumps(row), flush=True)
                        minute += 60
                    time.sleep(0.1)
            completed = json.loads((control / "idle-ready.json").read_bytes())
            expected = audio_seconds * 32000
            for key in ["captured_bytes", "confirmed_bytes", "verified_provider_bytes",
                        "accepted_bytes", "writer_received_bytes", "writer_completed_bytes"]:
                if completed.get(key) != expected:
                    raise RuntimeError(f"Exact functional PCM proof failed: {key}")
            if (
                completed["engine_lock"]["saved_bytes"] != expected
                or completed["engine_lock"]["confirmed_bytes"] != expected
                or completed.get("capture_frame_bytes") != 1280
                or completed.get("capture_frame_interval_ms") != 40
            ):
                raise RuntimeError("Durable cursor or original native frame contract mismatch.")
            cursors = completed.get("confirmed_cursors", [])
            if (
                completed["actual_capture_seconds"] < audio_seconds
                or not completed.get("capture_joined") or not completed.get("outbox_acknowledged")
                or not completed.get("history_saved_before_handoff") or not completed.get("polisher_exercised")
                or not cursors or cursors[-1] != expected
                or any(left >= right for left, right in zip(cursors, cursors[1:]))
                or completed["stream_starts"][0] != 0
                or (audio_seconds > 3000 and len(completed["stream_starts"]) < 2)
            ):
                raise RuntimeError("Duration/order/rotation/stop-join/outbox proof failed.")
            if any(completed["recovery_io"][key] != 0 for key in [
                "active_pinned_reads", "active_manifest_inspections", "retained_manifest_capacity_bytes",
            ]):
                raise RuntimeError("Joined functional capture still has pinned/alias ownership.")
            create(control / "finish", nonce.encode())
            process.wait(timeout=15)
            out, error = joined_logs()
            if process.returncode or error or "test result: ok. 1 passed;" not in out:
                raise RuntimeError(f"Native functional fixture failed: {process.returncode}; {error}")
            exit_proof = json.loads((control / "normal-exit.json").read_bytes())
            if not exit_proof.get("current_controller") or exit_proof.get("physical_ports") is not False:
                raise RuntimeError("Normal functional controller shutdown was not proved.")
            if hashlib.sha256(binary.read_bytes()).hexdigest() != digest:
                raise RuntimeError("Frozen native fixture changed during the functional run.")
            result = {
                "status": "pass", "scope": CONTROLLER_SCOPE, "source_revision": revision,
                "native_source_revision": native_revision or revision, "binary_sha256": digest,
                "pid": process.pid, "audio_seconds": audio_seconds, "actual_capture": completed,
                "functional_only": True, "resource_sampling": False, "whole_migration_acceptance": False,
                "precleanup_external_counts_cipher": "NOT_OBSERVED_EXTERNALLY; native final assertions precede ACK cleanup",
            }
            owned.preserve(process, failed=False)
        except BaseException as error:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=10)
            out, failure = joined_logs()
            create(directory / "failure.json", json.dumps({
                "status": "failed", "scope": CONTROLLER_SCOPE, "functional_only": True,
                "source_revision": revision, "native_source_revision": native_revision or revision,
                "binary_sha256": digest, "pid": process.pid, "error": str(error),
                "readiness_read": error.report if isinstance(error, ReadinessError) else None,
                "stdout": out, "stderr": failure, "whole_migration_acceptance": False,
            }, indent=2).encode())
            owned.preserve(process, failed=True)
            error.add_note(f"Native first-error evidence: {failure[:4096]}")
            raise
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=10)
            for reader in readers:
                reader.join(timeout=5)
            process.stdout.close()
            process.stderr.close()
    result["owned_control_directory_removed"] = not control.exists()
    create(directory / "functional-controller.json", json.dumps(result, indent=2).encode())
    print(json.dumps({key: value for key, value in result.items() if key != "actual_capture"}), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--oracle", type=Path, required=True)
    parser.add_argument("--output-directory", type=Path, required=True)
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--audio-seconds", type=int, default=3600)
    parser.add_argument("--idle-samples", type=int, default=301)
    parser.add_argument("--controller", action="store_true")
    parser.add_argument("--functional-only", action="store_true")
    parser.add_argument("--native-source-revision")
    args = parser.parse_args()
    if args.functional_only:
        if not args.controller:
            parser.error("--functional-only requires --controller.")
        run_functional(args.oracle.resolve(strict=True), args.output_directory, args.source_revision,
                       args.audio_seconds, args.native_source_revision)
    else:
        run(
            args.oracle.resolve(strict=True),
            args.output_directory,
            args.source_revision,
            args.audio_seconds,
            args.idle_samples,
            args.controller,
        )

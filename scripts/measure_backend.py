"""Linux authenticated in-memory HTTP profile, not deployed ACA/model acceptance."""

from __future__ import annotations

import argparse
import base64
import hashlib
import importlib.metadata
import json
import math
import os
import platform
import queue
import signal
import statistics
import struct
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from collections import deque
from datetime import UTC, datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import httpx
import jwt
from cryptography.hazmat.primitives.asymmetric import rsa

ROOT = Path(__file__).resolve().parents[1]
SCOPE = "authenticated-in-memory-http-no-ai-realtime"
OPERATIONS = {
    "create",
    "get_recording",
    "upload",
    "complete",
    "archive",
    "get_transcript",
    "list",
}


class Authority:
    def __init__(self) -> None:
        self.key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        public = self.key.public_key().public_numbers()

        def encoded(value: int) -> str:
            return (
                base64.urlsafe_b64encode(value.to_bytes((value.bit_length() + 7) // 8, "big"))
                .decode()
                .rstrip("=")
            )

        body = json.dumps(
            {
                "keys": [
                    {
                        "kty": "RSA",
                        "kid": "fixture-key",
                        "alg": "RS256",
                        "use": "sig",
                        "n": encoded(public.n),
                        "e": encoded(public.e),
                    }
                ]
            }
        ).encode()

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self) -> None:
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, format: str, *args: object) -> None:
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.url = f"http://127.0.0.1:{self.server.server_port}/jwks"
        self.token = self.make_token("fixture@example.invalid")
        self.completed_at = datetime.now(UTC).isoformat()

    def make_token(self, email: str) -> str:
        now = int(time.time())
        return jwt.encode(
            {
                "iss": "https://accounts.google.com",
                "aud": "fixture",
                "sub": "synthetic",
                "email": email,
                "email_verified": True,
                "iat": now,
                "exp": now + 3600,
            },
            self.key,
            algorithm="RS256",
            headers={"kid": "fixture-key"},
        )

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


class Fixture:
    def __init__(self, implementation: str, rust: Path, authority: Authority) -> None:
        if implementation not in {"python", "rust"} or rust.name not in {
            "cloud_contract",
            "cloud_contract.exe",
        }:
            raise ValueError("Only the named protocol fixture implementations are allowed.")
        arguments = (
            [str(rust), authority.url]
            if implementation == "rust"
            else [
                sys.executable,
                str(ROOT.joinpath("scripts", "backend_fixture.py")),
                authority.url,
            ]
        )
        environment = {
            key: value
            for key, value in os.environ.items()
            if not key.upper().startswith(("VR_", "AZURE_", "IDENTITY_", "VOICEPROMPT_", "GOOGLE_"))
        }
        environment["NO_PROXY"] = environment["no_proxy"] = "127.0.0.1,localhost"
        self.started = time.perf_counter()
        self.process = subprocess.Popen(
            arguments,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=environment,
            creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
        )
        self.logs: deque[bytes] = deque(maxlen=50)
        lines: queue.Queue[bytes] = queue.Queue(maxsize=1)

        def stdout() -> None:
            lines.put(self.process.stdout.readline())
            for line in self.process.stdout:
                self.logs.append(line)

        def stderr() -> None:
            for line in self.process.stderr:
                self.logs.append(line)

        self.readers = [
            threading.Thread(target=stdout, daemon=True),
            threading.Thread(target=stderr, daemon=True),
        ]
        for reader in self.readers:
            reader.start()
        try:
            line = lines.get(timeout=15)
            if not line:
                raise RuntimeError("Backend fixture exited before publishing its URL.")
            address = json.loads(line)["url"]
            parsed = httpx.URL(address)
            if (
                parsed.scheme != "http"
                or parsed.host != "127.0.0.1"
                or not parsed.port
                or parsed.userinfo
                or parsed.query
                or parsed.fragment
            ):
                raise RuntimeError("Fixture attempted to publish a non-loopback endpoint.")
            self.http = httpx.Client(base_url=address, trust_env=False, timeout=10)
            while True:
                try:
                    response = self.http.get("/health/ready")
                    if response.status_code == 200 and response.json() == {"status": "ok"}:
                        break
                except httpx.TransportError:
                    if self.process.poll() is not None or time.perf_counter() - self.started > 15:
                        raise
                if time.perf_counter() - self.started > 15:
                    raise TimeoutError("Actual HTTP readiness exceeded fifteen seconds.")
                time.sleep(0.001)
            self.startup_ms = (time.perf_counter() - self.started) * 1000
        except BaseException as error:
            if hasattr(self, "http"):
                self.http.close()
            self.process.kill()
            self.process.wait(timeout=10)
            self.finish_readers()
            error.add_note(self.diagnostics())
            raise

    def diagnostics(self) -> str:
        return b"".join(self.logs).decode(errors="replace")[-4096:]

    def finish_readers(self) -> None:
        for reader in self.readers:
            reader.join(timeout=5)
        self.process.stdout.close()
        self.process.stderr.close()

    def close(self) -> None:
        self.http.close()
        if self.process.poll() is not None:
            self.finish_readers()
            raise RuntimeError(
                f"Fixture exited prematurely: {self.process.returncode}; {self.diagnostics()}"
            )
        self.process.terminate()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=10)
            raise
        finally:
            self.finish_readers()
        expected = [0, -signal.SIGTERM] if os.name != "nt" else [0, 1]
        if self.process.returncode not in expected:
            raise RuntimeError(
                f"Fixture shutdown failed: {self.process.returncode}; {self.diagnostics()}"
            )

    def rss(self) -> int:
        if not sys.platform.startswith("linux"):
            raise RuntimeError("RSS collection requires Linux, never a success-shaped fallback.")
        if self.process.poll() is not None:
            raise RuntimeError("Fixture exited during RSS collection.")
        lines = Path(f"/proc/{self.process.pid}/smaps_rollup").read_text().splitlines()
        rss = [line.split() for line in lines if line.startswith("Rss:")]
        if (
            self.process.poll() is not None
            or len(rss) != 1
            or len(rss[0]) != 3
            or rss[0][2] != "kB"
        ):
            raise RuntimeError("Owned-process RSS could not be verified.")
        return int(rss[0][1]) * 1024


def percentile(values: list[float]) -> float:
    if not values:
        raise ValueError("A percentile requires actual samples.")
    return sorted(values)[math.ceil(len(values) * 0.95) - 1]


def wav() -> bytes:
    pcm = bytes(640)
    return (
        b"RIFF"
        + struct.pack("<I", len(pcm) + 36)
        + b"WAVEfmt "
        + struct.pack("<IHHIIHH", 16, 1, 1, 16000, 32000, 2, 16)
        + b"data"
        + struct.pack("<I", len(pcm))
        + pcm
    )


def controls(fixture: Fixture, authority: Authority) -> None:
    if fixture.http.get("/v1/transcripts").status_code != 401:
        raise RuntimeError("Fixture disabled bearer authentication.")
    forbidden = authority.make_token("other@example.invalid")
    if (
        fixture.http.get(
            "/v1/transcripts", headers={"Authorization": f"Bearer {forbidden}"}
        ).status_code
        != 403
    ):
        raise RuntimeError("Fixture disabled the allowlist.")
    prefix, signature = authority.token.rsplit(".", 1)
    corrupted = bytearray(base64.urlsafe_b64decode(signature + "=" * (-len(signature) % 4)))
    corrupted[0] ^= 1
    tampered = prefix + "." + base64.urlsafe_b64encode(corrupted).decode().rstrip("=")
    if (
        fixture.http.get(
            "/v1/transcripts", headers={"Authorization": f"Bearer {tampered}"}
        ).status_code
        != 401
    ):
        raise RuntimeError("Fixture accepted a forged RS256 signature.")
    for path, options in [
        ("/v1/realtime/negotiate", {"json": {}}),
        ("/v1/dictation/refine", {"json": {"text": "synthetic"}}),
        ("/v1/dictation/transcribe", {"content": wav(), "headers": {"Content-Type": "audio/wav"}}),
    ]:
        headers = {"Authorization": f"Bearer {authority.token}", **options.pop("headers", {})}
        response = fixture.http.post(path, headers=headers, **options)
        if response.status_code != 503:
            raise RuntimeError(
                f"The unavailable model/realtime profile differs at {path}: {response.status_code}"
            )


def exercise(fixture: Fixture, authority: Authority, index: int) -> dict[str, float]:
    headers = {"Authorization": f"Bearer {authority.token}"}
    timings = {}

    def request(name: str, method: str, path: str, status: int, **kwargs: object) -> dict:
        started = time.perf_counter()
        response = fixture.http.request(method, path, headers=headers, **kwargs)
        elapsed = (time.perf_counter() - started) * 1000
        if response.status_code != status:
            raise RuntimeError(
                f"{name} returned {response.status_code}, expected {status}: {response.text[:512]}"
            )
        timings[name] = elapsed
        return response.json()

    record = request(
        "create",
        "POST",
        "/v1/recordings",
        201,
        json={"client_recording_id": f"fixture-{index}", "refinement_enabled": False},
    )
    recording_id = record["recording_id"]
    if (
        str(uuid.UUID(recording_id)) != recording_id
        or record["client_recording_id"] != f"fixture-{index}"
    ):
        raise RuntimeError("The recording fixture did not persist the requested identity.")
    fetched = request("get_recording", "GET", f"/v1/recordings/{recording_id}", 200)
    if fetched != record or fetched["refinement_enabled"]:
        raise RuntimeError("Actual recording persistence was not exercised.")
    audio = wav()
    digest = "sha-256=:" + base64.b64encode(hashlib.sha256(audio).digest()).decode() + ":"
    started = time.perf_counter()
    uploaded = fixture.http.put(
        f"/v1/recordings/{recording_id}/chunks/0",
        content=audio,
        headers={**headers, "Content-Type": "audio/wav", "Content-Digest": digest},
    )
    timings["upload"] = (time.perf_counter() - started) * 1000
    if uploaded.status_code != 202:
        raise RuntimeError(f"Upload failed: {uploaded.status_code} {uploaded.text[:512]}")
    chunk = uploaded.json()
    if (
        chunk["recording_id"] != recording_id
        or chunk["index"] != 0
        or chunk["chunk_state"] != "accepted"
    ):
        raise RuntimeError("Actual chunk acceptance was not exercised.")
    completed = request(
        "complete", "POST", f"/v1/recordings/{recording_id}/complete", 202, json={"chunk_count": 1}
    )
    if (
        completed["state"] != "transcribing"
        or completed["progress"]["received_chunk_count"] != 1
        or completed["progress"]["expected_chunk_count"] != 1
        or completed["progress"]["transcribed_chunk_count"] != 0
    ):
        raise RuntimeError("The fixture faked a model completion.")
    audit_id = str(uuid.uuid5(uuid.NAMESPACE_URL, f"synthetic-performance-{index}"))
    request(
        "archive",
        "PUT",
        f"/v1/dictation/transcripts/{audit_id}",
        200,
        json={
            "text": "synthetic final",
            "raw_text": "synthetic original",
            "completed_at": authority.completed_at,
            "polished": False,
        },
    )
    transcript = request("get_transcript", "GET", f"/v1/transcripts/{audit_id}", 200)
    if transcript["body"] != "synthetic final" or transcript["raw_body"] != "synthetic original":
        raise RuntimeError("Actual transcript persistence was not exercised.")
    expires = datetime.fromisoformat(transcript["expires_at"].replace("Z", "+00:00"))
    if (expires - datetime.fromisoformat(authority.completed_at)).total_seconds() != 48 * 3600:
        raise RuntimeError("The fixture changed audit retention.")
    listing = request("list", "GET", "/v1/transcripts?limit=20", 200)
    if len(listing["items"]) != min(index + 1, 20):
        raise RuntimeError("The list fixture did not return the persisted workload.")
    return timings


def fingerprint(rust: Path) -> dict[str, str]:
    sources = {}
    paths = [
        *ROOT.joinpath("backend", "src").rglob("*.py"),
        ROOT.joinpath("backend", "uv.lock"),
        Path(__file__),
        ROOT.joinpath("scripts", "backend_fixture.py"),
    ]
    for path in sorted(paths):
        sources[str(path.relative_to(ROOT))] = hashlib.sha256(path.read_bytes()).hexdigest()
    sources["rust_binary"] = hashlib.sha256(rust.read_bytes()).hexdigest()
    return sources


def publish(output: Path, result: dict) -> None:
    with tempfile.NamedTemporaryFile(dir=output.parent, suffix=".tmp", delete=False) as stream:
        temporary = Path(stream.name)
        try:
            stream.write(json.dumps(result, indent=2).encode())
            stream.flush()
            os.fsync(stream.fileno())
        except BaseException:
            stream.close()
            temporary.unlink()
            raise
    try:
        os.link(temporary, output)
    finally:
        temporary.unlink()


def metrics(startup: dict, samples: dict, memory: dict, trials: int, count: int) -> tuple:
    expected = {"python", "rust"}
    if any(set(data) != expected for data in [startup, samples, memory]):
        raise ValueError("Both implementations must have complete measured data.")
    for name in expected:
        if len(startup[name]) != trials or len(samples[name]) != 3 or len(memory[name]) != 3:
            raise ValueError("Missing startup or load series.")
        times = list(startup[name])
        for series in samples[name]:
            if len(series) != count or any(set(row) != OPERATIONS for row in series):
                raise ValueError("Missing HTTP flows or operations.")
            times.extend(value for row in series for value in row.values())
        if any(not math.isfinite(value) or value <= 0 for value in times):
            raise ValueError("Timings must be actual positive finite observations.")
        if any(
            len(series) != 10 or any(type(value) is not int or value <= 0 for value in series)
            for series in memory[name]
        ):
            raise ValueError("RSS series must contain ten positive actual observations.")
    startups = {
        name: {
            "p95_ms": percentile(values),
            "mean_ms": statistics.mean(values),
            "max_ms": max(values),
        }
        for name, values in startup.items()
    }
    api = {
        name: [
            {operation: percentile([row[operation] for row in series]) for operation in OPERATIONS}
            for series in series_list
        ]
        for name, series_list in samples.items()
    }
    rss = {
        name: [statistics.mean(series) for series in series_list]
        for name, series_list in memory.items()
    }
    verdict = {
        "readiness": startups["rust"]["p95_ms"] <= 1000
        and startups["rust"]["p95_ms"] <= 0.50 * startups["python"]["p95_ms"],
        "rss": all(rss["rust"][index] <= 0.50 * rss["python"][index] for index in range(3)),
        "api_p95": all(
            api["rust"][index][operation] <= 1.10 * api["python"][index][operation]
            for index in range(3)
            for operation in OPERATIONS
        ),
    }
    return startups, api, rss, verdict


def check(rust: Path) -> None:
    authority = Authority()
    try:
        for implementation in ["python", "rust"]:
            fixture = Fixture(implementation, rust, authority)
            try:
                controls(fixture, authority)
                exercise(fixture, authority, 0)
                print(f"{implementation} real authenticated protocol fixture passed.", flush=True)
            finally:
                fixture.close()
    finally:
        authority.close()


def measure(rust: Path, output: Path, revision: str, trials: int, count: int) -> None:
    if not sys.platform.startswith("linux"):
        raise RuntimeError("Backend performance requires Linux.")
    if output.exists():
        raise FileExistsError(output)
    before = fingerprint(rust)
    authority = Authority()
    startup = {"python": [], "rust": []}
    rows = []
    samples = {"python": [], "rust": []}
    memory = {"python": [], "rust": []}
    memory_elapsed = {"python": [], "rust": []}
    load_pids = {}
    try:
        for trial in range(trials):
            for implementation in ["python", "rust"] if trial % 2 == 0 else ["rust", "python"]:
                fixture = Fixture(implementation, rust, authority)
                try:
                    startup[implementation].append(fixture.startup_ms)
                    rows.append(
                        {
                            "trial": trial + 1,
                            "implementation": implementation,
                            "pid": fixture.process.pid,
                            "startup_ms": fixture.startup_ms,
                        }
                    )
                finally:
                    fixture.close()
        fixtures = {}
        try:
            for implementation in ["python", "rust"]:
                fixture = Fixture(implementation, rust, authority)
                fixtures[implementation] = fixture
                load_pids[implementation] = fixture.process.pid
                controls(fixture, authority)
                exercise(fixture, authority, 0)
            for series in range(3):
                for implementation in ["python", "rust"] if series % 2 == 0 else ["rust", "python"]:
                    fixture = fixtures[implementation]
                    latencies = []
                    for index in range(count):
                        latencies.append(exercise(fixture, authority, 1 + series * count + index))
                    samples[implementation].append(latencies)
                    readings = []
                    elapsed = []
                    started = time.perf_counter()
                    for _ in range(10):
                        readings.append(fixture.rss())
                        elapsed.append(time.perf_counter() - started)
                        time.sleep(0.05)
                    memory[implementation].append(readings)
                    memory_elapsed[implementation].append(elapsed)
        finally:
            errors = []
            for fixture in fixtures.values():
                try:
                    fixture.close()
                except Exception as error:
                    errors.append(error)
            if errors:
                raise ExceptionGroup("Backend fixture cleanup failed.", errors)
    finally:
        authority.close()
    if fingerprint(rust) != before:
        raise RuntimeError("Measured implementation/bundle changed during collection.")
    startups, api, rss, verdict = metrics(startup, samples, memory, trials, count)
    result = {
        "schema_version": 1,
        "status": "complete",
        "scope": SCOPE,
        "source_revision": revision,
        "binary_source_and_lock_sha256": before,
        "platform": platform.platform(),
        "logical_cpu_count": os.cpu_count(),
        "python_runtime": sys.version,
        "python_packages": {
            name: importlib.metadata.version(name)
            for name in ["fastapi", "uvicorn", "httpx", "PyJWT", "cryptography"]
        },
        "whole_migration_acceptance": False,
        "measures_azure_private_networking": False,
        "measures_models_workers_or_realtime": False,
        "real_rs256_jwks_auth": True,
        "is_deployed_aca_or_container_profile": False,
        "exclusive_machine_not_asserted": True,
        "os_caches_not_reset": True,
        "github_actions": os.environ.get("GITHUB_ACTIONS") == "true",
        "idle_rss_is_short_post_load_observation": True,
        "rss_source": "/proc/<owned-pid>/smaps_rollup:Rss",
        "load_process_ids": load_pids,
        "corpus": {
            "completed_at": authority.completed_at,
            "wav_sha256": hashlib.sha256(wav()).hexdigest(),
            "recording_id_template": "fixture-{index}",
            "audit_id_template": "UUID5(URL,synthetic-performance-{index})",
            "final": "synthetic final",
            "original": "synthetic original",
        },
        "startup_trial_count": trials,
        "load_series": 3,
        "flows_per_series": count,
        "sufficient_for_this_protocol_profile": trials >= 30 and count >= 100,
        "startup": startups,
        "api_p95_ms_by_series": api,
        "rss_mean_bytes_by_series": rss,
        "profile_thresholds": verdict,
        "startup_samples": rows,
        "latency_samples_ms": samples,
        "rss_samples_bytes": memory,
        "rss_elapsed_seconds_by_series": memory_elapsed,
    }
    publish(output, result)
    print(
        json.dumps(
            {
                "scope": SCOPE,
                "profile_thresholds": verdict,
                "sufficient_for_this_protocol_profile": result[
                    "sufficient_for_this_protocol_profile"
                ],
                "whole_migration_acceptance": False,
            }
        ),
        flush=True,
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", type=Path, required=True)
    parser.add_argument("--check-fixtures", action="store_true")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--source-revision")
    parser.add_argument("--startup-trials", type=int, default=30)
    parser.add_argument("--flows-per-series", type=int, default=100)
    args = parser.parse_args()
    binary = args.rust.resolve(strict=True)
    if args.check_fixtures:
        check(binary)
    else:
        if (
            not args.output
            or not args.source_revision
            or not 1 <= args.startup_trials <= 100
            or not 1 <= args.flows_per_series <= 1000
        ):
            parser.error("Provide output/revision and bounded measurement counts.")
        measure(
            binary, args.output, args.source_revision, args.startup_trials, args.flows_per_series
        )

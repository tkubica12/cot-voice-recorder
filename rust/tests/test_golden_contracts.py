"""Exercise Rust black boxes against versioned, independently verified legacy data."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import queue
import re
import subprocess
import sys
import threading
import time
import unittest
import zlib
from datetime import UTC, datetime, timedelta
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

ROOT = Path(__file__).resolve().parents[2]
CORPUS = Path(__file__).with_name("golden")
MANIFEST = json.loads(CORPUS.joinpath("manifest.json").read_bytes())
BINARIES: dict[str, Path] = {}
JSON_MODES = {
    "cache",
    "jwt",
    "frame",
    "legacy-state",
    "normalize-batch",
}
HEADERS = ("Content-Type", "Cache-Control", "X-Request-Id", "Allow", "WWW-Authenticate")


def blob(key: str) -> bytes:
    if not re.fullmatch(r"[a-f0-9]{64}", key):
        raise ValueError("Invalid fixture blob identifier.")
    declared = MANIFEST["blobs"][key]["bytes"]
    if not 0 <= declared <= 64 * 1024 * 1024:
        raise ValueError("Fixture blob exceeds its declared bound.")
    decoder = zlib.decompressobj()
    data = decoder.decompress(CORPUS.joinpath(key + ".zlib").read_bytes(), declared + 1)
    if (
        len(data) != declared
        or not decoder.eof
        or decoder.unused_data
        or hashlib.sha256(data).hexdigest() != key
    ):
        raise ValueError("Fixture blob integrity failed.")
    return data


def shifted_dates(value, delta: timedelta):
    if isinstance(value, dict):
        return {key: shifted_dates(item, delta) for key, item in value.items()}
    if isinstance(value, list):
        return [shifted_dates(item, delta) for item in value]
    if isinstance(value, str) and re.fullmatch(
        r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?(?:Z|[+-]\d\d:\d\d)", value
    ):
        date = datetime.fromisoformat(value.replace("Z", "+00:00")) + delta
        result = date.isoformat(timespec="microseconds" if "." in value else "seconds")
        return result.replace("+00:00", "Z") if value.endswith("Z") else result
    return value


def archive_case(data: bytes, expected: bytes) -> tuple[bytes, dict]:
    request, response = json.loads(data), json.loads(expected)
    completed = datetime.fromisoformat(request["payload"]["completed_at"].replace("Z", "+00:00"))
    delta = datetime.now(UTC) - timedelta(hours=1) - completed
    request = shifted_dates(request, delta)
    response = shifted_dates(response, delta)
    if response["status"] == 200:
        envelope = shifted_dates(
            json.loads(base64.b64decode(response["envelope"], validate=True)), delta
        )
        encoded = json.dumps(
            envelope, ensure_ascii=False, sort_keys=True, separators=(",", ":")
        ).encode()
        old_digest = response["digest"]
        new_digest = hashlib.sha256(encoded).hexdigest()

        def replace_digest(value):
            if isinstance(value, dict):
                return {key: replace_digest(item) for key, item in value.items()}
            if isinstance(value, list):
                return [replace_digest(item) for item in value]
            return value.replace(old_digest, new_digest) if isinstance(value, str) else value

        response = replace_digest(response)
        response["envelope"] = base64.b64encode(encoded).decode()
    return json.dumps(request, ensure_ascii=False).encode(), response


class GoldenContracts(unittest.TestCase):
    def test_exact_legacy_prompts_and_recovery_snapshots(self) -> None:
        from legacy_reference import recovery_snapshot

        for name, key in MANIFEST["prompts"].items():
            current = ROOT.joinpath("rust", "cloud", "resources", name).read_text(encoding="utf-8")
            self.assertEqual(current.rstrip("\n").encode("utf-8"), blob(key))
        for case in MANIFEST["recovery"]:
            with self.subTest(mode=case["mode"]):
                snapshot, _ = recovery_snapshot(blob(case["plaintext_files"]["manifest.enc"]))
                self.assertEqual(snapshot, case["snapshot"])
        frames = MANIFEST["legacy_visual"]["frames"]
        self.assertEqual(len(frames), 15)
        for frame in frames:
            info = frame["metadata"]
            self.assertEqual(len(blob(frame["pixels"])), info["width"] * info["height"] * 4)

    def test_corpus_has_independent_provenance_and_original_cases(self) -> None:
        self.assertEqual(MANIFEST["schema"], 1)
        self.assertRegex(MANIFEST["source_commit"], r"^[a-f0-9]{40}$")
        self.assertTrue(MANIFEST["verification"]["all_passed"])
        self.assertGreaterEqual(MANIFEST["verification"]["legacy_comparison_tests"], 32)
        self.assertGreaterEqual(len(MANIFEST["records"]), 807)
        self.assertGreaterEqual(len(MANIFEST["http"]), 159)
        self.assertTrue(MANIFEST["source_sha256"])
        for key in MANIFEST["blobs"]:
            blob(key)

    def test_audio_client_and_cloud_wire_vectors(self) -> None:
        for record in MANIFEST["records"]:
            modes = record["args"]
            target = record["target"]
            executable = (
                BINARIES["audio"]
                if target == "csharp" and modes[0] in {"wrap", "chunk"}
                else BINARIES["client"]
                if target == "csharp"
                else BINARIES[target]
            )
            data, expected = blob(record["input"]), blob(record["stdout"])
            archive_expected = None
            if target == "cloud" and modes == ["archive"]:
                data, archive_expected = archive_case(data, expected)
            with self.subTest(target=target, mode=modes, input=record["input"]):
                result = subprocess.run(
                    [str(executable), *modes],
                    input=data,
                    capture_output=True,
                    timeout=120,
                    check=False,
                )
                self.assertEqual(result.returncode, record["returncode"], result.stderr)
                if result.returncode:
                    self.assertEqual(result.stdout, expected)
                    for diagnostic in record.get("required_diagnostics", []):
                        self.assertIn(diagnostic, result.stderr.decode("utf-8"))
                elif archive_expected is not None:
                    self.assertEqual(json.loads(result.stdout), archive_expected)
                elif target == "cloud" or modes[0] in JSON_MODES:
                    self.assertEqual(json.loads(result.stdout), json.loads(expected))
                elif modes[0] == "auth-url":
                    actual_url, expected_url = (
                        urlsplit(result.stdout.decode().strip()),
                        urlsplit(expected.decode().strip()),
                    )
                    self.assertEqual(
                        (actual_url.scheme, actual_url.netloc, actual_url.path),
                        (expected_url.scheme, expected_url.netloc, expected_url.path),
                    )
                    self.assertEqual(parse_qs(actual_url.query), parse_qs(expected_url.query))
                elif modes[0] in {"pkce", "callback", "refresh"}:
                    self.assertEqual(result.stdout.strip(), expected.strip())
                else:
                    self.assertEqual(result.stdout, expected)


class HttpFixture:
    def __init__(self) -> None:
        import httpx
        import jwt
        from cryptography.hazmat.primitives.asymmetric import rsa

        key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        public = key.public_key().public_numbers()

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

        class Authority(BaseHTTPRequestHandler):
            def do_GET(self) -> None:
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, format: str, *args: object) -> None:
                pass

        self.authority = ThreadingHTTPServer(("127.0.0.1", 0), Authority)
        self.thread = threading.Thread(target=self.authority.serve_forever, daemon=True)
        self.thread.start()
        self.process = subprocess.Popen(
            [str(BINARIES["http"]), f"http://127.0.0.1:{self.authority.server_port}/jwks"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            ready: queue.Queue[bytes] = queue.Queue()
            threading.Thread(
                target=lambda: ready.put(self.process.stdout.readline()), daemon=True
            ).start()
            line = ready.get(timeout=15)
            if not line:
                raise RuntimeError("The Rust HTTP fixture exited before readiness.")
            address = json.loads(line)["url"]
            parsed = urlsplit(address)
            if parsed.scheme != "http" or parsed.hostname != "127.0.0.1" or not parsed.port:
                raise ValueError("The HTTP contract fixture must bind loopback.")
            now = int(time.time())
            self.bearer = "Bearer " + jwt.encode(
                {
                    "iss": "https://accounts.google.com",
                    "aud": "fixture",
                    "sub": "synthetic",
                    "email": "fixture@example.invalid",
                    "email_verified": True,
                    "iat": now,
                    "exp": now + 3600,
                },
                key,
                algorithm="RS256",
                headers={"kid": "fixture-key"},
            )
            self.client = httpx.Client(base_url=address, trust_env=False, timeout=10)
        except BaseException:
            self.close()
            raise

    def close(self) -> None:
        if hasattr(self, "client"):
            self.client.close()
        if self.process.poll() is None:
            self.process.terminate()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=10)
        self.process.stdout.close()
        self.process.stderr.close()
        self.authority.shutdown()
        self.authority.server_close()
        self.thread.join(timeout=10)


class HttpGoldenContracts(unittest.TestCase):
    def test_openapi_structure_references_and_negative_wire_constraints(self) -> None:
        import yaml
        from jsonschema_lite import (
            assert_keywords_supported,
            assert_refs_resolve,
            iter_schema_nodes,
            validate,
        )

        spec = yaml.safe_load(
            ROOT.joinpath("openapi", "voice-recorder.yaml").read_text(encoding="utf-8")
        )
        self.assertTrue(str(spec["openapi"]).startswith("3.1"))
        self.assertTrue(spec["info"]["title"] and spec["info"]["version"])
        self.assertTrue(spec["paths"] and spec["components"]["schemas"])
        assert_refs_resolve(spec)
        assert_keywords_supported(spec)
        known = {"object", "array", "string", "integer", "number", "boolean", "null"}
        for pointer, schema in iter_schema_nodes(spec):
            declared = schema.get("type")
            if declared is not None:
                self.assertLessEqual(
                    set(declared if isinstance(declared, list) else [declared]), known, pointer
                )
        ids = [
            operation["operationId"]
            for item in spec["paths"].values()
            for method, operation in item.items()
            if method in {"get", "put", "post", "delete", "patch", "head", "options"}
        ]
        self.assertEqual(len(ids), len(set(ids)))
        schema = spec["components"]["schemas"]["DictationRefineResult"]
        for value in [
            {"text": "hello"},
            {"text": "hello", "edits": [{"original": "", "replacement": "hello"}]},
            {"text": "hello", "edits": [{"original": "a" * 257, "replacement": "hello"}]},
            {"text": "hello", "edits": [{"original": "helo", "replacement": "b" * 257}]},
            {"text": "hello", "edits": [{"original": "helo", "replacement": "hello", "start": 0}]},
            {"text": "hello", "edits": [{"original": "helo", "replacement": "hello"}] * 65},
        ]:
            self.assertTrue(validate(value, schema, spec))
        self.assertEqual(
            validate(
                {"text": "a test.", "edits": [{"original": "This is ", "replacement": ""}]},
                schema,
                spec,
            ),
            [],
        )
        recording = {
            "recording_id": "01234567-89ab-cdef-0123-456789abcdef",
            "client_recording_id": "abcdef01-2345-6789-abcd-ef0123456789",
            "state": "failed",
            "refine_model": "gpt-5.6-luna",
            "language": "cs",
            "progress": {
                "expected_chunk_count": 1,
                "received_chunk_count": 1,
                "transcribed_chunk_count": 0,
            },
            "transcript_id": None,
            "failure_reason": "not_a_real_reason",
            "created_at": "2026-09-03T12:00:00Z",
            "updated_at": "2026-09-03T12:00:00Z",
        }
        schema = spec["components"]["schemas"]["Recording"]
        self.assertTrue(validate(recording, schema, spec))
        recording["failure_reason"] = "missing_chunks"
        self.assertEqual(validate(recording, schema, spec), [])

    def test_signed_http_shapes_errors_lifecycle_and_rejected_body_keepalive(self) -> None:
        import yaml
        from jsonschema_lite import resolve_pointer, validate

        spec = yaml.safe_load(
            ROOT.joinpath("openapi", "voice-recorder.yaml").read_text(encoding="utf-8")
        )
        fixture = HttpFixture()
        self.addCleanup(fixture.close)
        identifiers: dict[str, str] = {}
        dates = [
            datetime.fromisoformat(case["options"]["json"]["completed_at"].replace("Z", "+00:00"))
            for case in MANIFEST["http"]
            if "completed_at" in case["options"].get("json", {})
        ]
        delta = datetime.now(UTC) - max(dates)

        def mapped(value):
            if isinstance(value, dict):
                return {key: mapped(item) for key, item in value.items()}
            if isinstance(value, list):
                return [mapped(item) for item in value]
            if isinstance(value, str):
                for old, new in identifiers.items():
                    value = value.replace(old, new)
            return value

        def compare(expected, actual, field: str = ""):
            if isinstance(expected, dict):
                self.assertIsInstance(actual, dict)
                self.assertEqual(set(actual), set(expected))
                for key in expected:
                    compare(expected[key], actual[key], key)
            elif isinstance(expected, list):
                self.assertIsInstance(actual, list)
                self.assertEqual(len(actual), len(expected))
                for left, right in zip(expected, actual, strict=True):
                    compare(left, right, field)
            elif field in {
                "created_at",
                "updated_at",
                "received_at",
                "started_at",
                "complete_requested_at",
            } and isinstance(expected, str):
                observed = datetime.fromisoformat(actual.replace("Z", "+00:00"))
                self.assertLess(abs((observed - datetime.now(UTC)).total_seconds()), 60)
            elif field in {"completed_at", "expires_at"} and isinstance(expected, str):
                self.assertEqual(
                    datetime.fromisoformat(actual.replace("Z", "+00:00")),
                    datetime.fromisoformat(expected.replace("Z", "+00:00")),
                )
            else:
                self.assertEqual(actual, expected)

        for index, case in enumerate(MANIFEST["http"]):
            path = mapped(case["path"])
            options = shifted_dates(case["options"], delta)
            headers = options.pop("headers")
            for name in headers:
                if headers[name] == "${VALID_BEARER}":
                    headers[name] = fixture.bearer
                elif headers[name] == "${INVALID_BEARER}":
                    headers[name] = "Bearer synthetic-invalid"
            if "content_blob" in options:
                options["content"] = blob(options.pop("content_blob"))
            with self.subTest(index=index, method=case["method"], path=path):
                response = fixture.client.request(case["method"], path, headers=headers, **options)
                self.assertEqual(response.status_code, case["status"], response.text)
                for name in HEADERS:
                    self.assertEqual(response.headers.get(name), case["headers"][name], name)
                expected = shifted_dates(json.loads(blob(case["body"])), delta)
                actual = response.json()
                if response.status_code >= 400:
                    schema = spec["components"]["schemas"]["Problem"]
                else:
                    matches = [
                        item
                        for template, item in spec["paths"].items()
                        if re.fullmatch(
                            re.sub(r"\\\{[^}]+\\\}", r"[^/]+", re.escape(template)),
                            urlsplit(path).path,
                        )
                    ]
                    self.assertEqual(len(matches), 1)
                    declared = matches[0][case["method"].lower()]["responses"][str(case["status"])]
                    if "$ref" in declared:
                        declared = resolve_pointer(spec, declared["$ref"])
                    schema = declared["content"]["application/json"]["schema"]
                self.assertEqual(
                    validate(actual, schema, spec),
                    validate(mapped(expected), schema, spec),
                )
                if case["status"] >= 400:
                    self.assertEqual(set(actual), set(expected))
                    for name in ("status", "code", "type", "instance", "trace_id"):
                        self.assertEqual(actual[name], mapped(expected[name]), name)
                    if "errors" in expected:
                        self.assertEqual(
                            [item["field"] for item in actual["errors"]],
                            [item["field"] for item in expected["errors"]],
                        )
                else:
                    if case["status"] == 201 and "recording_id" in expected:
                        identifiers[expected["recording_id"]] = actual["recording_id"]
                    compare(mapped(expected), actual)
        headers = {
            "Authorization": fixture.bearer,
            "X-Request-Id": "synthetic-openapi-contract",
        }
        created = fixture.client.post(
            "/v1/recordings",
            headers=headers,
            json={"client_recording_id": "abcdef01-2345-6789-abcd-ef0123456789"},
        )
        self.assertEqual(created.status_code, 201, created.text)
        recording_schema = spec["components"]["schemas"]["Recording"]
        self.assertEqual(validate(created.json(), recording_schema, spec), [])
        identifier = created.json()["recording_id"]
        self.assertEqual(created.headers["Location"], f"/v1/recordings/{identifier}")
        source = next(
            row for row in MANIFEST["http"] if row["method"] == "PUT" and row["status"] == 202
        )
        options = dict(source["options"])
        upload_headers = {**options["headers"], **headers}
        uploaded = fixture.client.put(
            f"/v1/recordings/{identifier}/chunks/0",
            headers=upload_headers,
            content=blob(options["content_blob"]),
        )
        self.assertEqual(uploaded.status_code, 202, uploaded.text)
        self.assertEqual(
            validate(uploaded.json(), spec["components"]["schemas"]["ChunkAccepted"], spec), []
        )
        completed = fixture.client.post(
            f"/v1/recordings/{identifier}/complete", headers=headers, json={"chunk_count": 1}
        )
        self.assertEqual(completed.status_code, 202, completed.text)
        self.assertEqual(validate(completed.json(), recording_schema, spec), [])


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--examples", type=Path, required=True)
    parser.add_argument("--without-http", action="store_true")
    args = parser.parse_args()
    examples = args.examples.resolve(strict=True)
    suffix = ".exe" if sys.platform == "win32" else ""
    BINARIES = {
        name: examples / (filename + suffix)
        for name, filename in {
            "audio": "audio_contract",
            "client": "client_contract",
            "cloud": "entity_contract",
            "http": "cloud_contract",
        }.items()
    }
    if any(not path.is_file() for path in BINARIES.values()):
        raise SystemExit("Build all four Rust contract examples before running these tests.")
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(GoldenContracts)
    if not args.without_http:
        suite.addTests(unittest.defaultTestLoader.loadTestsFromTestCase(HttpGoldenContracts))
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(0 if result.wasSuccessful() else 1)

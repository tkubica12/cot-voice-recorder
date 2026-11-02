"""Actual loopback Rust HTTP server versus current FastAPI, synthetic signed JWT only."""

from __future__ import annotations

import argparse
import base64
import json
import queue
import subprocess
import sys
import threading
import time
import unittest
from datetime import UTC, datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import httpx
import jwt
from cryptography.hazmat.primitives.asymmetric import rsa
from fastapi.testclient import TestClient

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT.joinpath("backend", "src")))
sys.path.insert(0, str(ROOT.joinpath("backend", "tests")))
from conftest import FakeClock, context, make_wav
from voice_recorder.ai.fakes import FakeDictationRefiner, FakeRefiner, FakeTranscriber
from voice_recorder.app import create_app
from voice_recorder.auth import AuthenticatedUser, StaticTokenVerifier
from voice_recorder.config import Settings
from voice_recorder.digest import compute_content_digest
from voice_recorder.realtime.memory import InMemoryRealtimeGateway
from voice_recorder.storage.memory import InMemoryBlobStore, InMemoryWorkQueue

ORACLE: Path


class HttpParityTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        public = key.public_key().public_numbers()

        def encoded(value: int) -> str:
            return (
                base64.urlsafe_b64encode(
                    value.to_bytes((value.bit_length() + 7) // 8, "big")
                )
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

        cls.authority = ThreadingHTTPServer(("127.0.0.1", 0), Authority)
        cls.authority_thread = threading.Thread(
            target=cls.authority.serve_forever, daemon=True
        )
        cls.authority_thread.start()
        cls.addClassCleanup(cls.authority.server_close)
        cls.addClassCleanup(cls.authority.shutdown)
        cls.process = subprocess.Popen(
            [str(ORACLE), f"http://127.0.0.1:{cls.authority.server_port}/jwks"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        cls.addClassCleanup(cls.stop_process)
        ready: queue.Queue[bytes] = queue.Queue()
        threading.Thread(
            target=lambda: ready.put(cls.process.stdout.readline()), daemon=True
        ).start()
        line = ready.get(timeout=15)
        if not line:
            raise AssertionError("Rust HTTP fixture exited before readiness.")
        address = json.loads(line)["url"]
        now = int(time.time())
        cls.token = jwt.encode(
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
        cls.rust = httpx.Client(base_url=address, trust_env=False, timeout=10)
        cls.addClassCleanup(cls.rust.close)
        cls.headers = {
            "Authorization": f"Bearer {cls.token}",
            "X-Request-Id": "synthetic-http-fixture",
        }
        settings = Settings(
            environment="test",
            allowlisted_email="fixture@example.invalid",
            log_level="ERROR",
        )
        ctx = context.__wrapped__(
            settings,
            FakeClock(datetime.now(UTC)),
            FakeTranscriber(),
            FakeRefiner(),
            InMemoryRealtimeGateway(),
            InMemoryWorkQueue(),
            InMemoryBlobStore(),
        )
        verifier = StaticTokenVerifier(
            valid_tokens={
                cls.token: AuthenticatedUser(
                    email="fixture@example.invalid",
                    subject="synthetic",
                    audience="fixture",
                )
            }
        )
        cls.reference = TestClient(
            create_app(
                context=ctx,
                verifier=verifier,
                dictation_transcriber=FakeTranscriber(),
                dictation_refiner=FakeDictationRefiner(),
            )
        )
        cls.reference.__enter__()
        cls.addClassCleanup(cls.reference.__exit__, None, None, None)

    @classmethod
    def stop_process(cls) -> None:
        cls.process.terminate()
        try:
            cls.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            cls.process.kill()
            cls.process.wait(timeout=10)
        cls.process.stdout.close()
        cls.process.stderr.close()

    def compare(
        self, method: str, path: str, **kwargs: object
    ) -> tuple[httpx.Response, httpx.Response]:
        headers = {**self.headers, **kwargs.pop("headers", {})}
        expected = self.reference.request(method, path, headers=headers, **kwargs)
        actual = self.rust.request(method, path, headers=headers, **kwargs)
        self.assertEqual(
            actual.status_code,
            expected.status_code,
            (method, path, expected.text, actual.text),
        )
        for header in [
            "Content-Type",
            "Cache-Control",
            "X-Request-Id",
            "Allow",
            "WWW-Authenticate",
        ]:
            self.assertEqual(
                actual.headers.get(header), expected.headers.get(header), (path, header)
            )
        if actual.status_code >= 400:
            left, right = actual.json(), expected.json()
            self.assertEqual(set(left), set(right), (path, left, right))
            for name in ["status", "code", "type", "instance", "trace_id"]:
                self.assertEqual(left[name], right[name], (path, name))
            if "errors" in right:
                self.assertEqual(
                    [item["field"] for item in left["errors"]],
                    [item["field"] for item in right["errors"]],
                )
        return actual, expected

    def test_error_contracts_body_query_digest_wav_and_media_types(self) -> None:
        for method, path, options in [
            ("GET", "/health/live", {}),
            ("GET", "/health/ready", {}),
            ("GET", "/missing", {}),
            ("DELETE", "/v1/recordings", {}),
            ("GET", "/v1/recordings/missing", {}),
            ("POST", "/v1/recordings", {"json": {}}),
            (
                "POST",
                "/v1/recordings",
                {"json": {"client_recording_id": "bad", "extra": True}},
            ),
            ("POST", "/v1/recordings", {"json": {"client_recording_id": 3}}),
            ("GET", "/v1/transcripts?limit=0", {}),
            ("GET", "/v1/transcripts?limit=101", {}),
            ("GET", "/v1/transcripts?limit=bad", {}),
            (
                "PUT",
                "/v1/recordings/missing/chunks/-1",
                {"content": b"", "headers": {"Content-Type": "audio/wav"}},
            ),
            (
                "PUT",
                "/v1/recordings/missing/chunks/0",
                {
                    "content": b"not wav",
                    "headers": {
                        "Content-Type": "audio/wav",
                        "Content-Digest": "invalid",
                    },
                },
            ),
            (
                "POST",
                "/v1/dictation/transcribe?language=invalid",
                {"content": make_wav(), "headers": {"Content-Type": "audio/wav"}},
            ),
            ("POST", "/v1/dictation/refine", {"json": {"text": ""}}),
            (
                "POST",
                "/v1/dictation/refine",
                {"json": {"text": "hello"}, "headers": {"Content-Encoding": "gzip"}},
            ),
            (
                "POST",
                "/v1/recordings",
                {
                    "json": {"client_recording_id": "wrong-media"},
                    "headers": {"Content-Type": "text/plain"},
                },
            ),
            (
                "POST",
                "/v1/recordings",
                {
                    "json": {"client_recording_id": "bad-auth"},
                    "headers": {"Authorization": "Bearer not-a-token"},
                },
            ),
            ("POST", "/v1/recordings/missing/complete", {"json": {"chunk_count": 0}}),
            (
                "PUT",
                "/v1/recordings/missing/chunks/0",
                {
                    "content": b"invalid wav",
                    "headers": {
                        "Content-Type": "audio/wav",
                        "Content-Digest": compute_content_digest(b"invalid wav"),
                    },
                },
            ),
            (
                "PUT",
                "/v1/recordings/missing/chunks/0",
                {
                    "content": make_wav(),
                    "headers": {
                        "Content-Type": "audio/wav",
                        "Content-Digest": compute_content_digest(b"mismatch"),
                    },
                },
            ),
        ]:
            with self.subTest(method=method, path=path):
                self.compare(method, path, **options)

    def test_recording_lifecycle_and_audit_get_list_have_actual_matching_shapes(
        self,
    ) -> None:
        recordings = []
        for client in [self.rust, self.reference]:
            response = client.post(
                "/v1/recordings",
                headers=self.headers,
                json={"client_recording_id": "lifecycle", "refinement_enabled": False},
            )
            self.assertEqual(response.status_code, 201, response.text)
            record = response.json()
            recordings.append(record)
            replay = client.post(
                "/v1/recordings",
                headers=self.headers,
                json={"client_recording_id": "lifecycle"},
            )
            self.assertEqual(replay.status_code, 200)
            self.assertEqual(replay.json()["recording_id"], record["recording_id"])
            wav = make_wav()
            uploaded = client.put(
                f"/v1/recordings/{record['recording_id']}/chunks/0",
                headers={
                    **self.headers,
                    "Content-Type": "audio/wav",
                    "Content-Digest": compute_content_digest(wav),
                },
                content=wav,
            )
            self.assertEqual(uploaded.status_code, 202, uploaded.text)
            complete = client.post(
                f"/v1/recordings/{record['recording_id']}/complete",
                headers=self.headers,
                json={"chunk_count": 1},
            )
            self.assertEqual(complete.status_code, 202, complete.text)
            self.assertEqual(complete.json()["state"], "transcribing")
        self.assertEqual(set(recordings[0]), set(recordings[1]))
        self.assertEqual(recordings[0]["progress"], recordings[1]["progress"])
        payload = {
            "text": "synthetic final",
            "raw_text": "synthetic original",
            "completed_at": datetime.now(UTC).isoformat(),
            "language": "cs",
            "polished": False,
        }
        actual, expected = self.compare(
            "PUT",
            "/v1/dictation/transcripts/0123456789abcdef0123456789abcdef",
            json=payload,
        )
        self.assertEqual(set(actual.json()), set(expected.json()))
        self.assertNotIn("character_count", actual.json())
        actual, expected = self.compare(
            "GET", "/v1/transcripts/01234567-89ab-cdef-0123-456789abcdef"
        )
        self.assertEqual(actual.json()["body"], expected.json()["body"])
        self.assertEqual(actual.json()["raw_body"], expected.json()["raw_body"])
        self.assertEqual(set(actual.json()), set(expected.json()))
        actual, expected = self.compare("GET", "/v1/transcripts")
        self.assertEqual(len(actual.json()["items"]), len(expected.json()["items"]))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--oracle", type=Path, required=True)
    args = parser.parse_args()
    ORACLE = args.oracle.resolve(strict=True)
    unittest.main(argv=[__file__], verbosity=2)

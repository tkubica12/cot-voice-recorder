"""Frozen legacy client wires and independent OS encrypted-cache contracts."""

from __future__ import annotations

import argparse
import base64
import json
import subprocess
import sys
import tempfile
import unittest
from datetime import UTC, datetime
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "tests"))
from legacy_reference import invoke

CLIENT: Path
PILOT: Path
REFERENCE: Path
VERIFIER = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"


def payload(value: object) -> bytes:
    return json.dumps(value, ensure_ascii=False).encode("utf-8")


def run(command: list[str], data: bytes = b"") -> subprocess.CompletedProcess[bytes]:
    if command[:2] == ["dotnet", str(REFERENCE)]:
        return invoke(command[2:], data, REFERENCE)
    return subprocess.run(command, input=data, capture_output=True, timeout=20, check=False)


class ClientParityTests(unittest.TestCase):
    def test_native_settings_files_are_compatible_with_csharp(self) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-settings-parity-") as directory:
            path = Path(directory).joinpath("settings.json")
            fixture = {
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
            for writer, reader in [
                ([str(PILOT), "--settings-save"], ["dotnet", str(REFERENCE), "settings-load"]),
                (["dotnet", str(REFERENCE), "settings-save"], [str(PILOT), "--settings-load"]),
            ]:
                saved = run([*writer, str(path)], payload(fixture))
                self.assertEqual(saved.returncode, 0, saved.stderr)
                loaded = run([*reader, str(path)])
                self.assertEqual(loaded.returncode, 0, loaded.stderr)
                self.assertEqual(json.loads(loaded.stdout), fixture)
                self.assertEqual(list(Path(directory).iterdir()), [path])
            path.write_bytes(b"corrupt synthetic settings")
            refused = run([str(PILOT), "--settings-load", str(path)])
            self.assertNotEqual(refused.returncode, 0)
            self.assertEqual(path.read_bytes(), b"corrupt synthetic settings")

    def test_native_history_files_are_compatible_with_csharp(self) -> None:
        now = datetime.now(UTC).isoformat()
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-history-parity-") as directory:
            path = Path(directory).joinpath("history.json")
            fixture = [
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
            for writer, reader in [
                ([str(PILOT), "--history-add"], ["dotnet", str(REFERENCE), "history-load"]),
                (["dotnet", str(REFERENCE), "history-add"], [str(PILOT), "--history-load"]),
            ]:
                saved = run([*writer, str(path)], payload(fixture))
                self.assertEqual(saved.returncode, 0, saved.stderr)
                loaded = run([*reader, str(path)])
                self.assertEqual(loaded.returncode, 0, loaded.stderr)
                entries = json.loads(loaded.stdout)
                self.assertEqual(len(entries), 1)
                self.assertEqual(entries[0]["body"], "synthetic body")
                self.assertEqual(entries[0]["raw_body"], "synthetic original")
                self.assertEqual(list(Path(directory).iterdir()), [path])

    def compare(self, mode: str, data: bytes, *, is_json: bool = True) -> None:
        rust = run([str(CLIENT), mode], data)
        csharp = run(["dotnet", str(REFERENCE), mode], data)
        self.assertEqual(rust.returncode, 0, rust.stderr)
        self.assertEqual(csharp.returncode, 0, csharp.stderr)
        if is_json:
            self.assertEqual(json.loads(rust.stdout), json.loads(csharp.stdout))
        else:
            self.assertEqual(rust.stdout.strip(), csharp.stdout.strip())

    def test_pkce_and_callbacks_match(self) -> None:
        self.compare("pkce", VERIFIER.encode("ascii"), is_json=False)
        for query in [
            "?code=abc123&state=expected",
            "code=abc%2B123&state=expected&scope=openid",
            "code=synthetic%20code&state=expected",
        ]:
            self.compare("callback", payload({"query": query, "state": "expected"}), is_json=False)
        for query in ["?code=x&state=other", "?code=x", "?state=expected", "?error=access_denied"]:
            data = payload({"query": query, "state": "expected"})
            self.assertNotEqual(run([str(CLIENT), "callback"], data).returncode, 0)
            self.assertNotEqual(run(["dotnet", str(REFERENCE), "callback"], data).returncode, 0)

    def test_authorization_urls_match_semantically_without_secrets(self) -> None:
        for hint in [None, "fixture@example.invalid"]:
            fixture = {
                "config": {
                    "installed": {"client_id": "fixture-id", "client_secret": "fixture-secret"}
                },
                "verifier": VERIFIER,
                "state": "synthetic-state",
                "redirect": "http://127.0.0.1:51234/",
                "login_hint": hint,
            }
            rust = run([str(CLIENT), "auth-url"], payload(fixture))
            csharp = run(["dotnet", str(REFERENCE), "auth-url"], payload(fixture))
            self.assertEqual(rust.returncode, 0, rust.stderr)
            self.assertEqual(csharp.returncode, 0, csharp.stderr)
            left, right = urlsplit(rust.stdout.decode().strip()), urlsplit(csharp.stdout.decode())
            self.assertEqual(
                (left.scheme, left.netloc, left.path), (right.scheme, right.netloc, right.path)
            )
            self.assertEqual(parse_qs(left.query), parse_qs(right.query))
            self.assertNotIn("fixture-secret", rust.stdout.decode())
            self.assertNotIn(VERIFIER, rust.stdout.decode())

    def test_jwt_claims_and_refresh_outcomes_match(self) -> None:
        for claims in [
            {},
            {"exp": 2_000_000_000, "aud": "aud", "email": "fixture@example.invalid", "sub": "s"},
            {"exp": 0, "aud": ["first", "second"]},
        ]:
            encoded = base64.urlsafe_b64encode(payload(claims)).rstrip(b"=")
            self.compare("jwt", b"e30." + encoded + b".")
        for response in [
            {},
            {"access_token": "synthetic-access"},
            {"id_token": "synthetic-id"},
            {"error": "invalid_grant"},
            {"error": "server_error"},
            {"error": "temporarily_unavailable"},
            {"error": "invalid_request"},
        ]:
            self.compare("refresh", payload(response), is_json=False)

    def test_pubsub_envelopes_match(self) -> None:
        event = {
            "event": "transcript.completed",
            "event_id": "e",
            "transcript_id": "t",
            "recording_id": "r",
            "completed_at": "2026-11-01T10:30:00Z",
            "preview": "synthetic preview",
        }
        for frame in [
            event,
            {"type": "system", "event": "connected", "userId": "fixture", "connectionId": "c"},
            {"type": "system", "event": "disconnected"},
            {"type": "ack", "ackId": 1, "success": True},
            {"type": "message", "from": "server", "dataType": "json", "data": event},
            {
                "type": "message",
                "from": "group",
                "group": "g",
                "fromUserId": "fixture",
                "dataType": "text",
                "data": json.dumps(event),
            },
            {"type": "message", "dataType": "binary", "data": "AAEC"},
            {"type": "unknown"},
        ]:
            self.compare("frame", payload(frame))

    def test_encrypted_token_files_work_in_both_directions_and_survive_replacement(self) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-client-cache-") as directory:
            path = Path(directory).joinpath("tokens.bin")
            for writer, reader in [
                ([str(PILOT), "--cache-save"], ["dotnet", str(REFERENCE), "cache-load"]),
                (["dotnet", str(REFERENCE), "cache-save"], [str(PILOT), "--cache-load"]),
            ]:
                for token in ["synthetic-first-refresh", "synthetic-rotated-refresh"]:
                    fixture = {
                        "RefreshToken": token,
                        "IdToken": "synthetic-id",
                        "IdTokenExpiresAt": "2026-11-01T10:30:00+00:00",
                        "Email": "fixture@example.invalid",
                    }
                    saved = run([*writer, str(path)], payload(fixture))
                    self.assertEqual(saved.returncode, 0, saved.stderr)
                    self.assertNotIn(token.encode(), path.read_bytes())
                    loaded = run([*reader, str(path)])
                    self.assertEqual(loaded.returncode, 0, loaded.stderr)
                    result = json.loads(loaded.stdout)
                    self.assertEqual(result["RefreshToken"], token)
                    self.assertEqual(result["IdToken"], fixture["IdToken"])
                    self.assertEqual(result["Email"], fixture["Email"])
                    self.assertEqual(
                        datetime.fromisoformat(result["IdTokenExpiresAt"].replace("Z", "+00:00")),
                        datetime.fromisoformat(fixture["IdTokenExpiresAt"]),
                    )
                    self.assertEqual(list(Path(directory).iterdir()), [path])
            path.write_bytes(b"corrupt synthetic data")
            failed = run([str(PILOT), "--cache-load", str(path)])
            self.assertNotEqual(failed.returncode, 0)
            self.assertTrue(path.exists())
            cleared = run([str(PILOT), "--cache-clear", str(path)])
            self.assertEqual(cleared.returncode, 0, cleared.stderr)
            self.assertFalse(path.exists())
            absent = run([str(PILOT), "--cache-load", str(path)])
            self.assertEqual(absent.returncode, 0, absent.stderr)
            self.assertIsNone(json.loads(absent.stdout))
            path.mkdir()
            refused = run(
                [str(PILOT), "--cache-save", str(path)], payload({"RefreshToken": "synthetic"})
            )
            self.assertNotEqual(refused.returncode, 0)
            self.assertTrue(path.is_dir())
            self.assertEqual(list(Path(directory).iterdir()), [path])
            path.rmdir()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--client", type=Path, required=True)
    parser.add_argument("--pilot", type=Path, required=True)
    parser.add_argument("--reference", type=Path, required=True)
    args = parser.parse_args()
    CLIENT = args.client.resolve(strict=True)
    PILOT = args.pilot.resolve(strict=True)
    REFERENCE = args.reference.resolve(strict=True)
    unittest.main(argv=[sys.argv[0]], verbosity=2)

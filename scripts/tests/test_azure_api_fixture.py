"""Verify adapter performance isolation and lifecycle without Docker or external accounts."""

from __future__ import annotations

import json
import ssl
import sys
import tempfile
import threading
import unittest
from contextlib import ExitStack
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import azure_api_fixture as fixture
import test_azure_rest


class AzureApiFixtureTests(unittest.TestCase):
    def config(self, root: Path) -> dict:
        config = {
            "scope": fixture.SCOPE,
            "key": "a" * 32,
            "blob": "https://127.0.0.1:12341/devstoreaccount1/",
            "queue": "https://127.0.0.1:12342/devstoreaccount1/",
            "table": "https://127.0.0.1:12343/devstoreaccount1/",
            "identity_endpoint": "http://127.0.0.1:12344/token",
            "identity_header": "b" * 32,
        }
        root.joinpath("server-cert.pem").write_bytes(b"synthetic certificate placeholder")
        root.joinpath("fixture.json").write_text(json.dumps(config))
        return config

    def test_exact_owned_namespace_and_loopback_tls_only_without_ambient_configuration(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-api-isolation-") as directory:
            root = Path(directory)
            original = self.config(root)
            self.assertEqual(fixture.load_fixture(root), original)
            for kind, value in [
                ("scope", "production"),
                ("key", "audio"),
                ("identity_header", ""),
                ("blob", "http://127.0.0.1:12341/devstoreaccount1/"),
                ("queue", "https://example.invalid:12342/devstoreaccount1/"),
                ("table", "https://user@127.0.0.1:12343/devstoreaccount1/"),
                ("table", "https://127.0.0.1:12343/devstoreaccount1/?query=value"),
                ("table", "https://127.0.0.1:12343/devstoreaccount1/#fragment"),
                ("identity_endpoint", "http://169.254.169.254/token"),
                ("identity_endpoint", "http://127.0.0.1:12344/another"),
            ]:
                with self.subTest(kind=kind, value=value):
                    root.joinpath("fixture.json").write_text(json.dumps({**original, kind: value}))
                    with self.assertRaises(ValueError):
                        fixture.load_fixture(root)
            root.joinpath("fixture.json").write_text(json.dumps({**original, "extra": "denied"}))
            with self.assertRaises(ValueError):
                fixture.load_fixture(root)
        first = fixture.resource_names("a" * 32)
        second = fixture.resource_names("b" * 32)
        self.assertFalse(set(first.values()) & set(second.values()))
        for value in ["", "../unsafe", "A" * 32, "a" * 31]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                fixture.resource_names(value)

    def test_synthetic_identity_is_explicit_scoped_cached_and_never_uses_cloud_sdk_discovery(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-api-identity-") as directory:
            root = Path(directory)
            config = self.config(root)
            identity = test_azure_rest.Identity(test_azure_rest.certificate(root))
            config["identity_endpoint"] = identity.endpoint
            config["identity_header"] = identity.header
            credential = fixture.LocalIdentity(config)
            try:
                first = credential.get_token("https://storage.azure.com/.default")
                self.assertEqual(first, credential.get_token("https://storage.azure.com/.default"))
                self.assertEqual(identity.resources, ["https://storage.azure.com/"])
                with self.assertRaises(ValueError):
                    credential.get_token("https://unapproved.example.invalid/.default")
                self.assertEqual(identity.resources, ["https://storage.azure.com/"])
            finally:
                credential.close()
                identity.close()

    def test_partial_namespace_setup_failure_deletes_only_owned_resources_and_root(self) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-api-cleanup-") as directory:
            root = Path(directory)
            self.config(root)
            blob, queue, table = Mock(), Mock(), Mock()
            queue.create_queue.side_effect = RuntimeError("synthetic setup refusal")
            identity = Mock(endpoint="http://127.0.0.1:12344/token", header="b" * 32)
            profile = fixture.AzureProfile(
                root,
                {key: self.config(root)[key] for key in ["blob", "queue", "table"]},
                identity,
                blob,
                queue,
                table,
            )
            with self.assertRaisesRegex(RuntimeError, "synthetic setup refusal"):
                profile.fixture("rust", Path("cloud_contract"), Mock())
            created = [call.args[0] for call in blob.create_container.call_args_list]
            deleted = [call.args[0] for call in blob.delete_container.call_args_list]
            self.assertEqual(deleted, list(reversed(created)))
            self.assertEqual(len(created), 3)
            self.assertFalse(any(root.glob("api-profile-*")))
            self.assertFalse(table.create_table.called)

    def test_actual_sdk_factory_uses_only_owned_https_paths_and_synthetic_bearer(self) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-api-sdk-") as directory:
            root = Path(directory)
            config = self.config(root)
            identity = test_azure_rest.Identity(test_azure_rest.certificate(root))
            requests = []

            class Handler(BaseHTTPRequestHandler):
                def reply(self) -> None:
                    size = int(self.headers.get("Content-Length", "0"))
                    self.rfile.read(size)
                    requests.append((self.command, self.path, self.headers.get("Authorization")))
                    self.send_response(201)
                    self.send_header("Content-Length", "0")
                    self.end_headers()

                do_POST = reply
                do_PUT = reply

                def log_message(self, format: str, *args: object) -> None:
                    pass

            server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
            tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            tls.load_cert_chain(root / "server-cert.pem", root / "server-key.pem")
            server.socket = tls.wrap_socket(server.socket, server_side=True)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            config.update(
                {
                    **dict.fromkeys(
                        ["blob", "queue", "table"],
                        f"https://127.0.0.1:{server.server_port}/devstoreaccount1/",
                    ),
                    "identity_endpoint": identity.endpoint,
                    "identity_header": identity.header,
                }
            )
            root.joinpath("fixture.json").write_text(json.dumps(config))
            try:
                with ExitStack() as resources:
                    actual = fixture.sdk_clients(root, resources)
                    self.assertEqual(actual["names"], fixture.resource_names(config["key"]))
                    self.assertEqual(len(requests), 7)
                    self.assertTrue(
                        all(path.startswith("/devstoreaccount1/") for _, path, _ in requests)
                    )
                    self.assertTrue(all(auth.startswith("Bearer ") for _, _, auth in requests))
                    self.assertEqual(identity.resources, ["https://storage.azure.com/"])
            finally:
                server.shutdown()
                server.server_close()
                thread.join(timeout=5)
                identity.close()

    def test_production_binary_and_unbounded_counts_are_rejected_before_docker(self) -> None:
        with (
            patch.object(test_azure_rest.sys, "platform", "linux"),
            patch.object(test_azure_rest, "docker") as docker,
        ):
            for name, count in [("voice-recorder", 30), ("cloud_contract", 0)]:
                with self.subTest(name=name), self.assertRaises(ValueError):
                    test_azure_rest.run(
                        Path("voiceprompt_cloud-synthetic"),
                        Path("unused"),
                        "synthetic",
                        Path(name),
                        count,
                        100,
                    )
            docker.assert_not_called()


if __name__ == "__main__":
    unittest.main(verbosity=2)

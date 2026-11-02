"""Local certificate/identity/endpoint guard tests; no Docker, Azure or user credentials."""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import httpx
import jwt
from cryptography import x509

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import test_azure_rest as fixture


class AzureRestFixtureTests(unittest.TestCase):
    def test_ephemeral_certificate_is_for_loopback_and_matches_the_generated_private_key(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-local-cert-") as directory:
            root = Path(directory)
            key = fixture.certificate(root)
            certificate = x509.load_pem_x509_certificate(
                root.joinpath("server-cert.pem").read_bytes()
            )
            self.assertEqual(
                certificate.public_key().public_numbers(), key.public_key().public_numbers()
            )
            self.assertTrue(
                certificate.extensions.get_extension_for_class(x509.BasicConstraints).value.ca
            )
            names = certificate.extensions.get_extension_for_class(
                x509.SubjectAlternativeName
            ).value
            self.assertIn("localhost", names.get_values_for_type(x509.DNSName))
            self.assertTrue(root.joinpath("server-key.pem").is_file())

    def test_identity_requires_exact_synthetic_header_scope_and_uami_and_emits_verifiable_token(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(prefix="VoicePrompt-local-mi-") as directory:
            key = fixture.certificate(Path(directory))
            identity = fixture.Identity(key)
            try:
                with httpx.Client(trust_env=False, timeout=5) as client:
                    query = {
                        "api-version": "2019-08-01",
                        "client_id": "synthetic-uami",
                        "resource": "https://storage.azure.com/",
                    }
                    self.assertEqual(client.get(identity.endpoint, params=query).status_code, 403)
                    response = client.get(
                        identity.endpoint,
                        params=query,
                        headers={"X-IDENTITY-HEADER": identity.header},
                    )
                    self.assertEqual(response.status_code, 200)
                    claims = jwt.decode(
                        response.json()["access_token"],
                        key.public_key(),
                        algorithms=["RS256"],
                        audience="https://storage.azure.com",
                    )
                    self.assertEqual(claims["tid"], "00000000-0000-0000-0000-000000000001")
                    bad = {**query, "resource": "https://unapproved.example.invalid/"}
                    self.assertEqual(
                        client.get(
                            identity.endpoint,
                            params=bad,
                            headers={"X-IDENTITY-HEADER": identity.header},
                        ).status_code,
                        403,
                    )
                    self.assertEqual(identity.resources, ["https://storage.azure.com/"])
            finally:
                identity.close()

    def test_mapped_endpoints_are_explicit_loopback_only_and_normal_binary_is_never_run(
        self,
    ) -> None:
        with patch.object(fixture, "docker", return_value="127.0.0.1:12345"):
            self.assertEqual(
                fixture.mapped_endpoint("synthetic", 10000),
                "https://127.0.0.1:12345/devstoreaccount1/",
            )
        for value in ["0.0.0.0:12345", "example.invalid:12345", "127.0.0.1:99999"]:
            with (
                self.subTest(value=value),
                patch.object(fixture, "docker", return_value=value),
                self.assertRaises(ValueError),
            ):
                fixture.mapped_endpoint("synthetic", 10000)
        with patch.object(fixture.sys, "platform", "linux"), self.assertRaises(ValueError):
            fixture.run(Path("voice-recorder"), Path("unused"), "synthetic")


if __name__ == "__main__":
    unittest.main(verbosity=2)

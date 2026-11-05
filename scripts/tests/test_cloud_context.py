"""Pure build-context allowlist tests; no Docker, credentials or cloud access."""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from prepare_cloud_context import EXACT, allowed


class CloudContextTests(unittest.TestCase):
    def test_only_required_manifests_and_production_sources_are_allowed(self):
        for name in EXACT:
            self.assertTrue(allowed(name), name)
        for name in (
            "rust/src/lib.rs",
            "rust/cloud/src/main.rs",
            "rust/client-protocol/src/auth.rs",
            "rust/windows-pilot/src/lib.rs",
        ):
            self.assertTrue(allowed(name), name)

    def test_private_runtime_data_test_fixtures_and_untracked_shapes_are_excluded(self):
        for name in (
            ".env",
            ".secrets/client.json",
            ".git/config",
            "session-state/token.txt",
            "rust/target/release/voice-recorder",
            "rust/tests/golden/manifest.json",
            "rust/cloud/src/api_tests.rs",
            "rust/cloud/src/tests.rs",
            "rust/cloud/src/tests/private.rs",
            "rust/cloud/examples/cloud_contract.rs",
            "rust/cloud/src/settings.json",
            "rust/cloud/resources/user-transcript.txt",
            "rust/windows-pilot/installer/google-desktop-client.json",
            "../rust/src/lib.rs",
            "/rust/src/lib.rs",
            "rust\\src\\lib.rs",
            "rust/cloud/src/../../private.rs",
            "backup/tokens.bin",
            "cert.pem",
        ):
            self.assertFalse(allowed(name), name)


if __name__ == "__main__":
    unittest.main(verbosity=2)

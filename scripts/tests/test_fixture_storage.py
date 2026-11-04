"""Pure bounded storage-root guards; no native capture, GUI or resource sampler."""
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch, Mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import synthetic_fixture_root as storage
import measure_synthetic_capture as capture


class Exited:
    def poll(self):
        return 0


class Running:
    def poll(self):
        return None


class FixtureStorageTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="VoicePrompt-storage-guards-")
        self.area = Path(self.directory.name).resolve()
        self.base = self.area / "neutral"
        self.output = self.area / "artifacts"
        self.base.mkdir()
        self.output.mkdir()
        self.override = patch.object(storage, "temp_base", return_value=self.base)
        self.override.start()

    def tearDown(self):
        self.override.stop()
        self.directory.cleanup()

    def test_new_GUID_marker_and_separation_are_canonical(self):
        with storage.FixtureRoot(self.output) as owned:
            self.assertEqual(owned.path.parent, self.base)
            self.assertEqual(owned.path.name, storage.PREFIX + owned.nonce)
            self.assertFalse(owned.path.is_relative_to(self.output))
            self.assertFalse(self.output.is_relative_to(owned.path))
            owned.owned()
            owned.preserve(Exited(), failed=False)
        self.assertFalse(owned.path.exists())
        self.assertTrue((self.output / "control-state" / "migration-fixture.json").exists())

    def test_snapshot_is_rejected_before_child_exit(self):
        owned = storage.FixtureRoot(self.output)
        with self.assertRaises(RuntimeError):
            owned.preserve(Running(), failed=True)
        with self.assertRaises(RuntimeError):
            owned.__exit__(None, None, None)
        self.assertTrue(owned.path.exists())
        self.assertFalse((self.output / "failure-state").exists())

    def test_log_reader_start_failure_cannot_leave_mutating_native_child(self):
        process = Mock(pid=123)
        process.poll.return_value = None
        reader = Mock()
        reader.start.side_effect = RuntimeError("controlled reader startup failure")
        with self.assertRaises(RuntimeError):
            capture.start_readers(process, [reader])
        process.kill.assert_called_once()
        process.wait.assert_called_once_with(timeout=10)
        process.stdout.close.assert_called_once()
        process.stderr.close.assert_called_once()

    def test_failure_copy_and_hashes_precede_only_owned_cleanup(self):
        unrelated = self.base / "unrelated"
        unrelated.mkdir()
        (unrelated / "keep").write_bytes(b"untouched")
        remove = storage.shutil.rmtree
        owned = storage.FixtureRoot(self.output)
        (owned.path / "encrypted-fixture.bin").write_bytes(b"synthetic ciphertext")
        owned.preserve(Exited(), failed=True)
        def checked(path):
            self.assertEqual(path, owned.path)
            self.assertEqual(storage.hashes(path), storage.hashes(self.output / "failure-state"))
            self.assertTrue((self.output / "state-hashes.json").exists())
            remove(path)
        with patch.object(storage.shutil, "rmtree", side_effect=checked):
            owned.__exit__(None, None, None)
        self.assertEqual((unrelated / "keep").read_bytes(), b"untouched")

    def test_failed_preservation_retains_root_and_first_error(self):
        owned = storage.FixtureRoot(self.output)
        original = RuntimeError("original native failure")
        with patch.object(storage.shutil, "copytree", side_effect=PermissionError("fixture")):
            with self.assertRaises(PermissionError):
                owned.preserve(Exited(), failed=True)
        self.assertFalse(owned.__exit__(RuntimeError, original, None))
        self.assertTrue(owned.path.exists())
        self.assertIn(str(owned.path), original.__notes__[0])

    def test_changed_marker_after_snapshot_refuses_cleanup(self):
        owned = storage.FixtureRoot(self.output)
        owned.preserve(Exited(), failed=True)
        (owned.path / "migration-fixture.json").write_text(json.dumps({
            "scope": storage.SCOPE, "instance_key": "wrong",
        }))
        with self.assertRaises(ValueError):
            owned.__exit__(None, None, None)
        self.assertTrue(owned.path.exists())

    def test_changed_data_after_snapshot_refuses_cleanup(self):
        owned = storage.FixtureRoot(self.output)
        owned.preserve(Exited(), failed=True)
        (owned.path / "unexpected").write_bytes(b"not in snapshot")
        with self.assertRaises(RuntimeError):
            owned.__exit__(None, None, None)
        self.assertTrue(owned.path.exists())

    def test_replaced_directory_identity_is_not_same_owned_leaf(self):
        owned = storage.FixtureRoot(self.output)
        moved = self.base / "preserved-original"
        owned.path.rename(moved)
        owned.path.mkdir()
        storage.write_json(owned.path / "migration-fixture.json",
                           {"scope": storage.SCOPE, "instance_key": owned.nonce})
        with self.assertRaises(ValueError):
            owned.owned()
        self.assertTrue(moved.exists())

    def test_incomplete_copied_snapshot_cannot_authorize_cleanup(self):
        owned = storage.FixtureRoot(self.output)
        (owned.path / "encrypted-fixture.bin").write_bytes(b"synthetic ciphertext")
        copy = storage.shutil.copytree
        def damaged(source, target):
            result = copy(source, target)
            (target / "encrypted-fixture.bin").write_bytes(b"corrupt")
            return result
        with patch.object(storage.shutil, "copytree", side_effect=damaged), self.assertRaises(RuntimeError):
            owned.preserve(Exited(), failed=True)
        self.assertIsNone(owned.saved_hashes)
        self.assertTrue(owned.path.exists())

    def test_artifacts_repo_and_OneDrive_bases_are_rejected(self):
        cases = [self.output, self.area / ".copilot" / "Temp", self.area / "OneDrive - Fixture" / "Temp"]
        for i, base in enumerate(cases):
            base.mkdir(parents=True, exist_ok=True)
            evidence = self.area / f"evidence-{i}"
            evidence.mkdir()
            # An evidence parent containing the mutable base must be rejected.
            if base == self.output:
                evidence = self.output
            with patch.object(storage, "temp_base", return_value=base), self.assertRaises(ValueError):
                storage.FixtureRoot(evidence)
        repo = self.area / "repo"
        repo.mkdir()
        with patch.object(storage, "REPOSITORY", repo), patch.object(storage, "temp_base", return_value=repo), self.assertRaises(ValueError):
            storage.FixtureRoot(self.output)

    def test_actual_junction_or_symlink_base_is_rejected(self):
        target = self.area / "target"
        link = self.area / "link"
        target.mkdir()
        (target / "keep").write_bytes(b"safe")
        if os.name == "nt":
            quote = lambda path: str(path).replace("'", "''")
            subprocess.run(["pwsh", "-NoProfile", "-NonInteractive", "-Command",
                            f"New-Item -ItemType Junction -Path '{quote(link)}' -Target '{quote(target)}' | Out-Null"], check=True)
        else:
            link.symlink_to(target, target_is_directory=True)
        with patch.object(storage, "temp_base", return_value=link), self.assertRaises(ValueError):
            storage.FixtureRoot(self.output)
        self.assertEqual((target / "keep").read_bytes(), b"safe")

    def test_reparse_child_refuses_snapshot(self):
        owned = storage.FixtureRoot(self.output)
        target = self.area / "other"
        target.mkdir()
        link = owned.path / "untrusted"
        if os.name == "nt":
            quote = lambda path: str(path).replace("'", "''")
            subprocess.run(["pwsh", "-NoProfile", "-NonInteractive", "-Command",
                            f"New-Item -ItemType Junction -Path '{quote(link)}' -Target '{quote(target)}' | Out-Null"], check=True)
        else:
            link.symlink_to(target, target_is_directory=True)
        with self.assertRaises(ValueError):
            owned.preserve(Exited(), failed=True)
        self.assertTrue(owned.path.exists())
        self.assertFalse((self.output / "failure-state").exists())


if __name__ == "__main__":
    unittest.main()

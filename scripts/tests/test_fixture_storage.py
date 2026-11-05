"""Pure bounded storage-root guards; no native capture, GUI or resource sampler."""
import json
import errno
import hashlib
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

    def ready_case(self, name="ready-evidence"):
        evidence = self.output / name
        evidence.mkdir()
        owned = storage.FixtureRoot(evidence)
        binary = self.area / "owned-native.bin"
        binary.write_bytes(b"synthetic immutable native image")
        digest = hashlib.sha256(binary.read_bytes()).hexdigest()
        process = Mock(pid=123)
        process.poll.return_value = None
        ready = {
            "scope": capture.CONTROLLER_SCOPE, "pid": process.pid, "physical_ports": False,
            "network_or_auth": False, "full_client_profile": False,
            "current_controller": True, "renderer": False, "main_ui_or_tray": False,
            "real_auth_or_pubsub": False, "polisher_and_outbox": True,
        }
        return owned, binary, digest, process, ready

    def test_ready_atomic_publication_missing_and_shared_read_window_retries_only_within_deadline(self):
        owned, binary, digest, process, ready = self.ready_case()
        path = owned.path / "ready.json"
        checks = 0
        def publish_on_poll():
            nonlocal checks
            checks += 1
            if checks == 2:
                temporary = owned.path / "ready.tmp"
                temporary.write_bytes(json.dumps(ready).encode())
                temporary.replace(path)
            return None
        process.poll.side_effect = publish_on_poll
        original = Path.open
        denied = False
        def sharing_window(candidate, *args, **kwargs):
            nonlocal denied
            if candidate == path and not denied:
                denied = True
                raise PermissionError(errno.EACCES, "controlled ready sharing window")
            return original(candidate, *args, **kwargs)
        with patch.object(Path, "open", sharing_window):
            self.assertEqual(capture.wait_ready(owned, process, binary, digest, 1), ready)
        report = json.loads((owned.evidence / "readiness-read.json").read_bytes())
        self.assertEqual(report["attempts"], 3)
        self.assertEqual(report["first_error"]["errno"], errno.ENOENT)
        self.assertEqual(report["last_error"]["errno"], errno.EACCES)
        self.assertEqual(report["binary_sha256"], digest)
        self.assertTrue(report["root_marker_and_directory_identity_verified"])

    def test_ready_permanent_denial_and_process_exit_report_first_last_errors_without_success(self):
        for exited in [False, True]:
            owned, binary, digest, process, ready = self.ready_case(str(exited))
            path = owned.path / "ready.json"
            path.write_bytes(json.dumps(ready).encode())
            if exited:
                process.poll.side_effect = [None, 7, 7]
            original = Path.open
            def denied(candidate, *args, **kwargs):
                if candidate == path:
                    raise PermissionError(errno.EACCES, "controlled stable denial")
                return original(candidate, *args, **kwargs)
            with patch.object(Path, "open", denied), self.assertRaises(capture.ReadinessError) as failed:
                capture.wait_ready(owned, process, binary, digest, 0.02)
            report = failed.exception.report
            self.assertEqual(report["first_error"]["errno"], errno.EACCES)
            self.assertEqual(report["last_error"]["errno"], errno.EACCES)
            self.assertGreaterEqual(report["attempts"], 1)
            self.assertEqual(report["native_exit_code"], 7 if exited else None)
            self.assertIn("exited" if exited else "deadline", report["reason"])
            self.assertFalse((owned.evidence / "readiness-read.json").exists())
            self.assertFalse((owned.path / "begin").exists())

    def test_ready_invalid_json_pid_bool_shape_hash_and_nonce_fail_without_cached_success(self):
        for case in ["json", "pid", "shape", "bool", "hash", "nonce"]:
            owned, binary, digest, process, ready = self.ready_case(case)
            if case == "pid":
                ready["pid"] += 1
            elif case == "shape":
                ready["unexpected"] = True
            elif case == "bool":
                ready["physical_ports"] = 0
            elif case == "hash":
                binary.write_bytes(b"different native")
            elif case == "nonce":
                (owned.path / "migration-fixture.json").write_text(json.dumps({
                    "scope": storage.SCOPE, "instance_key": "wrong",
                }))
            (owned.path / "ready.json").write_bytes(
                b"{" if case == "json" else json.dumps(ready).encode())
            with self.assertRaises(ValueError if case == "nonce" else capture.ReadinessError):
                capture.wait_ready(owned, process, binary, digest, 1)
            self.assertFalse((owned.evidence / "readiness-read.json").exists())
            self.assertFalse((owned.path / "begin").exists())

    @unittest.skipUnless(os.name == "nt", "Actual own Windows file-sharing guard.")
    def test_actual_owned_ready_exclusive_handle_is_observed_then_released_without_native_capture(self):
        import ctypes
        from ctypes import wintypes
        owned, binary, digest, process, ready = self.ready_case()
        path = owned.path / "ready.json"
        path.write_bytes(json.dumps(ready).encode())
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                                      ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
        kernel.CreateFileW.restype = wintypes.HANDLE
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        kernel.CloseHandle.restype = wintypes.BOOL
        handle = kernel.CreateFileW(str(path), 0x80000000, 0, None, 3, 0, None)
        self.assertNotEqual(handle, ctypes.c_void_p(-1).value)
        observed = []
        original = Path.open
        def read_and_release(candidate, *args, **kwargs):
            nonlocal handle
            try:
                return original(candidate, *args, **kwargs)
            except PermissionError as error:
                if candidate == path:
                    observed.append(error.errno)
                    self.assertTrue(kernel.CloseHandle(handle))
                    handle = None
                raise
        try:
            with patch.object(Path, "open", read_and_release):
                self.assertEqual(capture.wait_ready(owned, process, binary, digest, 1), ready)
        finally:
            if handle is not None:
                kernel.CloseHandle(handle)
        self.assertEqual(observed, [errno.EACCES])
        report = json.loads((owned.evidence / "readiness-read.json").read_bytes())
        self.assertEqual(report["attempts"], 2)
        self.assertEqual(report["first_error"]["errno"], errno.EACCES)

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

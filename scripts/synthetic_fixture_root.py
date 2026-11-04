"""Owned mutable fixture storage, separate from logs and preserved only after exit."""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import stat
import sys
import tempfile
import uuid
from pathlib import Path

REPOSITORY = Path(__file__).resolve().parents[1]
SCOPE = "full-client-signed-out-fixture"
PREFIX = "VoicePrompt-fixture-"
MAX_SNAPSHOT_BYTES = 320 * 1024 * 1024


def guard(path: Path) -> None:
    for candidate in (path, *path.parents):
        info = candidate.lstat()
        if stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & 0x400:
            raise ValueError("Synthetic storage refuses symlink/reparse paths.")


def temp_base() -> Path:
    base = (
        Path(os.environ["LOCALAPPDATA"]).joinpath("Temp")
        if os.name == "nt"
        else Path(tempfile.gettempdir())
    )
    guard(base)
    return base.resolve(strict=True)


def hashes(root: Path) -> dict[str, str]:
    guard(root)
    result = {}
    total = 0
    for path in root.rglob("*"):
        guard(path)
        info = path.lstat()
        if stat.S_ISDIR(info.st_mode):
            continue
        if not stat.S_ISREG(info.st_mode):
            raise ValueError("Synthetic snapshot refuses non-regular files.")
        total += info.st_size
        if total > MAX_SNAPSHOT_BYTES:
            raise ValueError("Synthetic snapshot exceeds its bounded owned-data size.")
        digest = hashlib.sha256()
        with path.open("rb") as source:
            for block in iter(lambda: source.read(256 * 1024), b""):
                digest.update(block)
        result[str(path.relative_to(root))] = digest.hexdigest()
    return result


def write_json(path: Path, value: object) -> None:
    with path.open("x", encoding="utf-8") as output:
        json.dump(value, output, indent=2)
        output.flush()
        os.fsync(output.fileno())


class FixtureRoot:
    def __init__(self, evidence: Path):
        guard(evidence)
        self.evidence = evidence.resolve(strict=True)
        guard(self.evidence)
        base = temp_base()
        guard(base)
        base = base.resolve(strict=True)
        self.nonce = uuid.uuid4().hex
        self.path = base / (PREFIX + self.nonce)
        forbidden = [REPOSITORY.resolve(strict=True), self.evidence]
        if any(self.path.is_relative_to(parent) or parent.is_relative_to(self.path)
               for parent in forbidden):
            raise ValueError("Mutable synthetic data must be outside repository/artifacts.")
        if any(part.lower() == ".copilot" or part.lower().startswith("onedrive")
               for part in self.path.parts):
            raise ValueError("Mutable synthetic data must be outside Copilot/OneDrive.")
        self.path.mkdir()
        guard(self.path)
        self.resolved = self.path.resolve(strict=True)
        info = self.path.stat()
        self.identity = (info.st_dev, info.st_ino)
        write_json(self.path / "migration-fixture.json",
                   {"scope": SCOPE, "instance_key": self.nonce})
        self.saved_hashes: dict[str, str] | None = None
        write_json(self.evidence / "data-root.json", {
            "root": str(self.path), "nonce": self.nonce, "scope": SCOPE,
            "mutable_data_outside_artifacts": True, "cleanup_requires_exit_snapshot": True,
        })

    def __enter__(self) -> FixtureRoot:
        return self

    def owned(self) -> None:
        guard(self.path)
        if self.path.name != PREFIX + self.nonce or self.path.resolve(strict=True) != self.resolved:
            raise ValueError("Synthetic storage ownership/path changed.")
        info = self.path.stat()
        if (info.st_dev, info.st_ino) != self.identity:
            raise ValueError("Synthetic storage directory identity changed.")
        marker = self.path / "migration-fixture.json"
        guard(marker)
        if marker.stat().st_size > 1024:
            raise ValueError("Synthetic storage marker exceeds bounds.")
        if json.loads(marker.read_bytes()) != {"scope": SCOPE, "instance_key": self.nonce}:
            raise ValueError("Synthetic storage ownership marker changed.")

    def preserve(self, process, failed: bool) -> None:
        if process.poll() is None:
            raise RuntimeError("Cannot copy mutable synthetic data before process exit.")
        self.owned()
        before = hashes(self.path)
        destination = self.evidence / ("failure-state" if failed else "control-state")
        shutil.copytree(self.path, destination)
        if hashes(destination) != before or hashes(self.path) != before:
            raise RuntimeError("Post-exit synthetic snapshot changed or is incomplete.")
        write_json(self.evidence / "state-hashes.json", {
            "after_process_exit": True, "snapshot": destination.name, "files": before,
        })
        self.saved_hashes = before

    def __exit__(self, kind, error, traceback) -> bool:
        removed = False
        try:
            if self.saved_hashes is None:
                raise RuntimeError("No verified post-exit snapshot; owned data retained.")
            self.owned()
            if hashes(self.path) != self.saved_hashes:
                raise RuntimeError("Owned data changed after preservation; cleanup refused.")
            shutil.rmtree(self.path)
            removed = True
            write_json(self.evidence / "data-cleanup.json", {
                "owned_root": str(self.path), "removed": True,
                "snapshot_verified_before_cleanup": True, "no_broad_temp_cleanup": True,
            })
        except (OSError, ValueError, RuntimeError) as cleanup:
            note = (
                f"Synthetic cleanup record failed after verified removal of {self.path}: {cleanup}"
                if removed else f"Synthetic fixture root retained at {self.path}: {cleanup}"
            )
            print(note, file=sys.stderr, flush=True)
            if error is not None:
                error.add_note(note)
            else:
                raise
        return False

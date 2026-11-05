"""Export an allowlisted, immutable tracked-source-only cloud build context."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
from pathlib import Path, PurePosixPath

EXACT = {
    ".dockerignore",
    ".cargo/config.toml",
    "rust/Cargo.toml",
    "rust/Cargo.lock",
    "rust/client-protocol/Cargo.toml",
    "rust/windows-pilot/Cargo.toml",
    "rust/windows-pilot/build.rs",
    "rust/cloud/Cargo.toml",
    "rust/cloud/Dockerfile",
    "rust/cloud/resources/dictation-system.txt",
    "rust/cloud/resources/recording-system.txt",
}
SOURCES = ("rust/src/", "rust/client-protocol/src/", "rust/windows-pilot/src/", "rust/cloud/src/")


def allowed(name: str) -> bool:
    path = PurePosixPath(name)
    if path.is_absolute() or ".." in path.parts or "\\" in name:
        return False
    if name in EXACT:
        return True
    return (
        name.startswith(SOURCES)
        and path.suffix == ".rs"
        and not path.name.endswith("_tests.rs")
        and path.name != "tests.rs"
        and not {"tests", "examples", "target", ".secrets"}.intersection(path.parts)
    )


def prepare(repo: Path, revision: str, output: Path, proof: Path) -> None:
    if not re.fullmatch(r"[a-f0-9]{40}", revision):
        raise ValueError("An exact trusted commit SHA is required.")
    if not output.is_absolute() or output.exists() or proof.exists():
        raise ValueError("Fresh explicit build context and proof paths are required.")
    tree = subprocess.check_output(["git", "-C", str(repo), "ls-tree", "-r", "-z", revision])
    selected = {}
    for row in tree.split(b"\0"):
        if not row:
            continue
        meta, raw_name = row.split(b"\t", 1)
        name = raw_name.decode("utf-8")
        if not allowed(name):
            continue
        mode, kind, oid = meta.decode().split()
        if mode not in {"100644", "100755"} or kind != "blob":
            raise ValueError("Symlinks/submodules are forbidden in the build context.")
        selected[name] = oid
    if not EXACT.issubset(selected):
        raise ValueError("Required tracked build inputs are missing.")
    output.mkdir()
    files = {}
    for name, oid in sorted(selected.items()):
        data = subprocess.check_output(["git", "-C", str(repo), "cat-file", "blob", oid])
        target = output.joinpath(*PurePosixPath(name).parts)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        files[name] = {"sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)}
    with proof.open("x", encoding="utf-8") as file:
        json.dump(
            {
                "scope": "immutable-tracked-production-source-only",
                "revision": revision,
                "files": files,
                "untracked_files_or_runtime_config_copied": False,
            },
            file,
            indent=2,
        )
    print(json.dumps({"revision": revision, "source_files": len(files), "secret_or_data_files": 0}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--proof", type=Path, required=True)
    args = parser.parse_args()
    prepare(Path(__file__).resolve().parents[1], args.revision, args.output, args.proof)

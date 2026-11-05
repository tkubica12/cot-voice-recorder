"""Test-only frozen wire fixtures and an independent Windows OS oracle.

There is no application model, audio engine, auth service or network client here.
File operations are restricted to the existing synthetic test directories.
"""

from __future__ import annotations

import base64
import hashlib
import json
import struct
import subprocess
from datetime import UTC, datetime, timedelta
from pathlib import Path

from test_golden_contracts import MANIFEST, blob

_RECORDS = {
    (tuple(row["args"]), row["input"]): row
    for row in MANIFEST["records"]
    if row["target"] == "csharp"
}
_EPOCH = 621355968000000000


def _scope(path: Path) -> None:
    if not path.is_absolute():
        raise ValueError("An absolute synthetic fixture path is required.")
    if not path.name.startswith("VoicePrompt-") and not any(
        parent.name.startswith("VoicePrompt-") for parent in path.parents
    ):
        raise ValueError("The reference helper only accepts synthetic fixture directories.")


def _os(oracle: Path, mode: str, data: bytes) -> bytes:
    result = subprocess.run(
        ["dotnet", str(oracle), mode],
        input=data,
        capture_output=True,
        timeout=60,
        check=False,
    )
    if result.returncode:
        raise ValueError(result.stderr.decode("utf-8", errors="replace"))
    return result.stdout


def _date(ticks: int) -> str:
    seconds, fraction = divmod(ticks - _EPOCH, 10000000)
    date = datetime(1970, 1, 1, tzinfo=UTC) + timedelta(seconds=seconds)
    suffix = f"{fraction:07}".rstrip("0")
    return date.strftime("%Y-%m-%dT%H:%M:%S") + ("." + suffix if suffix else "") + "+00:00"


def recovery_snapshot(data: bytes) -> tuple[dict, list[tuple[int, bytes, int]]]:
    position = 0

    def read(format: str):
        nonlocal position
        size = struct.calcsize("<" + format)
        if position + size > len(data):
            raise ValueError("Truncated frozen recovery wire value.")
        result = struct.unpack_from("<" + format, data, position)[0]
        position += size
        return result

    def binary() -> bytes:
        nonlocal position
        length = read("i")
        if length < 0 or position + length > len(data):
            raise ValueError("Invalid frozen recovery field length.")
        value = data[position : position + length]
        position += length
        return value

    def text() -> str:
        return binary().decode("utf-8")

    if read("I") != 0x5650524A:
        raise ValueError("Invalid recovery wire magic.")
    version = read("I")
    if version not in (1, 2, 3, 4):
        raise ValueError("Unsupported recovery wire version.")
    result = {
        "Id": text(),
        "CreatedAt": _date(read("q")),
        "UpdatedAt": _date(read("q")),
        "Language": text(),
        "CapturedBytes": read("Q"),
        "Tail": base64.b64encode(binary()).decode(),
        "CaptureComplete": bool(read("?")),
        "Chunks": [],
        "Context": "",
        "Streaming": version == 4,
        "ConfirmedBytes": 0,
        "ConfirmedText": "",
        "AuditText": None,
        "AuditCompletedAt": None,
        "AuditPolished": False,
        "UsedStreaming": False,
        "UsedFallback": False,
        "LastSegmentWords": 0,
    }
    count = read("I")
    if count > 100000 or count > len(data) // 46:
        raise ValueError("Invalid recovery fixture chunk count.")
    audio = []
    for _ in range(count):
        index, overlap, has_text = read("I"), read("?"), read("?")
        value = text() if has_text else None
        size, checksum = read("I"), binary()
        if len(checksum) != 32:
            raise ValueError("Invalid recovery audio digest.")
        end = read("Q") if version >= 3 else 0
        result["Chunks"].append(
            {
                "Index": index,
                "OverlapsPrevious": overlap,
                "Text": value,
                "EndByte": end,
            }
        )
        audio.append((index, checksum, size))
    if version >= 2:
        result["Context"] = text()
    if version == 4:
        result["ConfirmedBytes"], result["ConfirmedText"] = read("Q"), text()
        result["UsedStreaming"], result["UsedFallback"] = read("?"), read("?")
        result["LastSegmentWords"] = read("I")
        if read("?"):
            result["AuditText"] = text()
            result["AuditCompletedAt"] = _date(read("q"))
            result["AuditPolished"] = read("?")
    if position != len(data):
        raise ValueError("Unexpected trailing recovery fixture bytes.")
    return result, audio


def invoke(args: list[str], data: bytes, oracle: Path) -> subprocess.CompletedProcess[bytes]:
    try:
        mode = args[0]
        if mode in {"protect", "unprotect"}:
            output = _os(oracle, mode, data)
        elif mode == "recovery-create":
            root = Path(args[1])
            _scope(root)
            fixture = next(row for row in MANIFEST["recovery"] if row["mode"] == args[2])
            session = root / fixture["snapshot"]["Id"]
            session.mkdir()
            for name, key in fixture["plaintext_files"].items():
                session.joinpath(name).write_bytes(_os(oracle, "protect", blob(key)))
            output = json.dumps(fixture["snapshot"]).encode()
        elif mode == "recovery-inspect":
            root = Path(args[1])
            _scope(root)
            if len(args[2]) != 32 or any(char not in "0123456789abcdef" for char in args[2]):
                raise ValueError("Invalid synthetic recovery ID.")
            session = root / args[2]
            snapshot, chunks = recovery_snapshot(
                _os(oracle, "unprotect", session.joinpath("manifest.enc").read_bytes())
            )
            for index, expected, size in chunks:
                path = session / f"{index:08}.wav.enc"
                if path.is_file():
                    wav = _os(oracle, "unprotect", path.read_bytes())
                    if len(wav) != size or hashlib.sha256(wav).digest() != expected:
                        raise ValueError("Synthetic recovery WAV digest mismatch.")
            output = json.dumps(snapshot).encode()
        elif mode in {
            "settings-save",
            "settings-load",
            "history-add",
            "history-load",
            "cache-save",
            "cache-load",
            "cache-clear",
        }:
            path = Path(args[1])
            _scope(path)
            kind = mode.split("-", 1)[0]
            if mode == "cache-clear":
                path.unlink(missing_ok=True)
                output = b""
            elif mode.endswith("-load"):
                if kind == "cache" and not path.exists():
                    output = b"null"
                else:
                    wire = path.read_bytes()
                    if kind == "cache":
                        wire = _os(oracle, "unprotect", wire)
                    value = json.loads(wire)
                    template = json.loads(blob(MANIFEST["persistence"][kind]["wire"]))
                    left = value[0] if kind == "history" else value
                    right = template[0] if kind == "history" else template
                    if set(left) != set(right):
                        raise ValueError(
                            "Persistence keys differ from the actual legacy wire golden."
                        )
                    output = json.dumps(value).encode()
            else:
                value = json.loads(data)
                fixture = MANIFEST["persistence"][kind]
                template = json.loads(blob(fixture["wire"]))
                expected = json.loads(blob(fixture["input"]))
                if kind == "history":
                    if len(value) != 1:
                        raise ValueError(
                            "Only the versioned single-entry history case is supported."
                        )
                    for field in ("completed_at", "cached_at"):
                        template[0][field] = value[0][field]
                        expected[0][field] = value[0][field]
                elif kind == "cache":
                    for field in ("RefreshToken", "IdToken", "IdTokenExpiresAt", "Email"):
                        template[field] = value[field]
                        expected[field] = value[field]
                if value != expected:
                    raise ValueError("The fixture differs from its versioned legacy input.")
                wire = json.dumps(template, ensure_ascii=False).encode()
                path.write_bytes(_os(oracle, "protect", wire) if kind == "cache" else wire)
                output = b""
        else:
            row = _RECORDS.get((tuple(args), hashlib.sha256(data).hexdigest()))
            if row is None:
                raise ValueError("No independently verified legacy vector exists for this input.")
            return subprocess.CompletedProcess(args, row["returncode"], blob(row["stdout"]), b"")
        return subprocess.CompletedProcess(args, 0, output, b"")
    except (OSError, ValueError, StopIteration, KeyError) as error:
        return subprocess.CompletedProcess(
            args, 2, b"", f"Frozen compatibility fixture: {error}".encode()
        )

"""Export owned native/actual-WPF Pbgra32 fixture rasters without desktop capture."""

from __future__ import annotations

import argparse
import hashlib
import json
import struct
import zlib
from pathlib import Path

STATES = (
    "listening-empty",
    "listening-preview",
    "listening-wrap",
    "finishing",
    "saved",
)
DPIS = (96, 144, 192)


def publish(path: Path, data: bytes) -> None:
    with path.open("xb") as file:
        file.write(data)


def png(width: int, height: int, rgba: bytes) -> bytes:
    def chunk(kind: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + kind
            + data
            + struct.pack(">I", zlib.crc32(kind + data))
        )

    stride = width * 4
    raw = b"".join(
        b"\0" + rgba[row * stride : (row + 1) * stride] for row in range(height)
    )
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw))
        + chunk(b"IEND", b"")
    )


def load(root: Path, name: str) -> tuple[dict, bytes, bytes]:
    metadata = json.loads(root.joinpath(name + ".json").read_bytes())
    width, height, dpi = (metadata[key] for key in ("width", "height", "dpi"))
    if dpi not in DPIS or (width, height) != (440 * dpi // 96, 112 * dpi // 96):
        raise ValueError("Unexpected overlay geometry or DPI.")
    if metadata["pixel_format"] != "Pbgra32":
        raise ValueError("Unexpected overlay pixel format.")
    premultiplied = root.joinpath(name + ".bgra").read_bytes()
    if len(premultiplied) != width * height * 4:
        raise ValueError("Missing or truncated overlay pixels.")
    rgba = bytearray(len(premultiplied))
    for offset in range(0, len(rgba), 4):
        b, g, r, a = premultiplied[offset : offset + 4]
        if max(b, g, r) > a:
            raise ValueError("Invalid premultiplied alpha.")
        rgba[offset : offset + 4] = bytes(
            0 if not a else min(255, (c * 255 + a // 2) // a) for c in (r, g, b)
        ) + bytes([a])
    return metadata, premultiplied, bytes(rgba)


def compare(native: Path, reference: Path, output: Path) -> None:
    if not output.is_dir() or any(output.iterdir()):
        raise ValueError("Output must be an existing empty artifact directory.")
    for root, scope in (
        (native, "native-overlay-visual-fixture"),
        (reference, "reference-overlay-visual-fixture"),
    ):
        marker = json.loads(root.joinpath("overlay-fixture.json").read_bytes())
        result = json.loads(root.joinpath("result.json").read_bytes())
        if marker != {"scope": scope} or result.get("scope") != scope:
            raise ValueError("Visual fixture ownership scope differs.")
        if (
            result.get("complete") is not True
            or result.get("physical_ports") is not False
        ):
            raise ValueError("Incomplete or non-isolated visual fixture.")
    reports = []
    for state in STATES:
        for dpi in DPIS:
            name = f"{state}-{dpi}"
            info, left, left_rgba = load(native, name)
            expected, right, right_rgba = load(reference, name)
            if info != expected:
                raise ValueError(
                    f"{name}: actual native/reference frame semantics differ."
                )
            width, height = info["width"], info["height"]
            publish(output / f"{name}-native.png", png(width, height, left_rgba))
            publish(output / f"{name}-reference.png", png(width, height, right_rgba))
            combined = bytearray()
            stride = width * 4
            for row in range(height):
                for source in (left, right):
                    for offset in range(row * stride, (row + 1) * stride, 4):
                        b, g, r, a = source[offset : offset + 4]
                        combined.extend((r + 255 - a, g + 255 - a, b + 255 - a, 255))
            publish(
                output / f"{name}-native-left-reference-right.png",
                png(width * 2, height, bytes(combined)),
            )
            reports.append(
                {
                    "state": state,
                    "dpi": dpi,
                    "native_bgra_sha256": hashlib.sha256(left).hexdigest(),
                    "reference_bgra_sha256": hashlib.sha256(right).hexdigest(),
                    "mean_abs_channel_error_0_255": sum(
                        abs(a - b) for a, b in zip(left, right, strict=True)
                    )
                    / len(left),
                    "different_pixel_fraction": sum(
                        left[i : i + 4] != right[i : i + 4]
                        for i in range(0, len(left), 4)
                    )
                    / (width * height),
                }
            )
    publish(
        output / "comparison.json",
        json.dumps(
            {
                "scope": "owned-raster-export-not-physical-or-desktop-capture",
                "source": "actual native renderer and actual WPF DictationIndicator",
                "pairs": reports,
                "image_order": "native left, reference right",
                "visual_acceptance": "requires inspection; no pixel-identical or performance claim",
            },
            indent=2,
        ).encode(),
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--native", type=Path, required=True)
    parser.add_argument("--reference", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    compare(
        args.native.resolve(strict=True),
        args.reference.resolve(strict=True),
        args.output.resolve(strict=True),
    )

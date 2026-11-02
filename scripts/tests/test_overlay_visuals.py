"""Pure export guards; no window, account, capture, or clipboard APIs."""

import importlib.util
import json
import struct
import tempfile
import unittest
import zlib
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "overlay_visuals",
    Path(__file__).resolve().parents[1] / "compare_overlay_visuals.py",
)
assert SPEC is not None and SPEC.loader is not None
VISUALS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VISUALS)


class OverlayVisualGuards(unittest.TestCase):
    def test_png_encoder_preserves_exact_rgba_and_dimensions(self) -> None:
        pixels = bytes((255, 0, 0, 128, 0, 255, 0, 255))
        encoded = VISUALS.png(2, 1, pixels)
        self.assertEqual(encoded[:8], b"\x89PNG\r\n\x1a\n")
        self.assertEqual(struct.unpack_from(">II", encoded, 16), (2, 1))
        offset = 8
        image = b""
        while offset < len(encoded):
            length = struct.unpack_from(">I", encoded, offset)[0]
            kind = encoded[offset + 4 : offset + 8]
            data = encoded[offset + 8 : offset + 8 + length]
            crc = struct.unpack_from(">I", encoded, offset + 8 + length)[0]
            self.assertEqual(crc, zlib.crc32(kind + data))
            if kind == b"IDAT":
                image += data
            offset += length + 12
        self.assertEqual(zlib.decompress(image), b"\0" + pixels)

    def test_bad_geometry_or_truncated_pixels_are_explicit_errors(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            metadata = {
                "width": 440,
                "height": 112,
                "dpi": 96,
                "pixel_format": "Pbgra32",
            }
            root.joinpath("case.json").write_text(json.dumps(metadata))
            root.joinpath("case.bgra").write_bytes(b"short")
            with self.assertRaisesRegex(ValueError, "truncated"):
                VISUALS.load(root, "case")
            metadata["width"] = 441
            root.joinpath("case.json").write_text(json.dumps(metadata))
            with self.assertRaisesRegex(ValueError, "geometry"):
                VISUALS.load(root, "case")

    def test_incomplete_fixture_never_publishes_success_comparison(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            native, reference, output = (
                root / name for name in ("native", "reference", "output")
            )
            for path in (native, reference, output):
                path.mkdir()
            scope = "native-overlay-visual-fixture"
            native.joinpath("overlay-fixture.json").write_text(
                json.dumps({"scope": scope})
            )
            native.joinpath("result.json").write_text(
                json.dumps({"scope": scope, "complete": False, "physical_ports": False})
            )
            with self.assertRaisesRegex(ValueError, "Incomplete"):
                VISUALS.compare(native, reference, output)
            self.assertEqual(list(output.iterdir()), [])


if __name__ == "__main__":
    unittest.main()

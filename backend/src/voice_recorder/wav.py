"""Validation of uploaded WAV chunks: RIFF/WAVE container and PCM properties.

We parse just enough of the RIFF structure to assert the audio is 16 kHz, mono,
16-bit PCM, without pulling in an audio dependency. Any deviation raises a 422.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass

from .problems import ValidationProblemError


@dataclass(frozen=True, slots=True)
class WavProperties:
    audio_format: int
    channels: int
    sample_rate: int
    bits_per_sample: int
    data_bytes: int


def _fail(message: str) -> ValidationProblemError:
    return ValidationProblemError(message, errors=[{"field": "body", "message": message}])


def parse_wav(data: bytes) -> WavProperties:
    """Parse and structurally validate a RIFF/WAVE byte string.

    Raises:
        ValidationProblemError: when the container is not a valid WAVE file.
    """
    if len(data) < 44:
        raise _fail("WAV payload is too small to contain a valid header.")
    if data[0:4] != b"RIFF":
        raise _fail("Missing RIFF header.")
    if data[8:12] != b"WAVE":
        raise _fail("Missing WAVE format marker.")

    fmt: WavProperties | None = None
    data_bytes: int | None = None
    offset = 12
    total = len(data)
    while offset + 8 <= total:
        chunk_id = data[offset : offset + 4]
        (chunk_size,) = struct.unpack_from("<I", data, offset + 4)
        body_start = offset + 8
        if chunk_id == b"fmt ":
            if chunk_size < 16 or body_start + 16 > total:
                raise _fail("Malformed fmt chunk.")
            audio_format, channels, sample_rate, _byte_rate, _block_align, bits = (
                struct.unpack_from("<HHIIHH", data, body_start)
            )
            fmt = WavProperties(
                audio_format=audio_format,
                channels=channels,
                sample_rate=sample_rate,
                bits_per_sample=bits,
                data_bytes=0,
            )
        elif chunk_id == b"data":
            data_bytes = chunk_size
        # Chunks are word-aligned (padded to even length).
        advance = chunk_size + (chunk_size & 1)
        offset = body_start + advance

    if fmt is None:
        raise _fail("WAV is missing its fmt chunk.")
    if data_bytes is None:
        raise _fail("WAV is missing its data chunk.")
    # PCM audio format code is 1; anything else is not raw PCM.
    if fmt.audio_format != 1:
        raise _fail("WAV must be uncompressed 16-bit PCM (format code 1).")
    return WavProperties(
        audio_format=fmt.audio_format,
        channels=fmt.channels,
        sample_rate=fmt.sample_rate,
        bits_per_sample=fmt.bits_per_sample,
        data_bytes=data_bytes,
    )


def validate_wav(
    data: bytes,
    *,
    sample_rate: int,
    channels: int,
    bits_per_sample: int,
) -> WavProperties:
    """Validate PCM properties against the expected capture format."""
    props = parse_wav(data)
    problems: list[dict[str, str]] = []
    if props.channels != channels:
        problems.append(
            {"field": "channels", "message": f"expected {channels}, got {props.channels}"}
        )
    if props.sample_rate != sample_rate:
        problems.append(
            {
                "field": "sample_rate",
                "message": f"expected {sample_rate}, got {props.sample_rate}",
            }
        )
    if props.bits_per_sample != bits_per_sample:
        problems.append(
            {
                "field": "bits_per_sample",
                "message": f"expected {bits_per_sample}, got {props.bits_per_sample}",
            }
        )
    if problems:
        raise ValidationProblemError(
            "WAV PCM properties do not match the required capture format.",
            errors=problems,
        )
    return props

import struct

import pytest

from voice_recorder.problems import ValidationProblemError
from voice_recorder.wav import parse_wav, validate_wav

from .conftest import make_wav

DEFAULTS = {"sample_rate": 16_000, "channels": 1, "bits_per_sample": 16}


def test_valid_wav_parses() -> None:
    props = parse_wav(make_wav(640))
    assert props.sample_rate == 16_000
    assert props.channels == 1
    assert props.bits_per_sample == 16
    assert props.data_bytes == 640


def test_validate_accepts_expected_format() -> None:
    validate_wav(make_wav(), **DEFAULTS)


def test_rejects_non_riff() -> None:
    with pytest.raises(ValidationProblemError):
        parse_wav(b"NOTAWAVEFILE" + bytes(64))


def test_rejects_too_small() -> None:
    with pytest.raises(ValidationProblemError):
        parse_wav(b"RIFF")


def test_rejects_wrong_sample_rate() -> None:
    wav = _wav(sample_rate=8000)
    with pytest.raises(ValidationProblemError) as exc:
        validate_wav(wav, **DEFAULTS)
    assert any(e["field"] == "sample_rate" for e in exc.value.errors or [])


def test_rejects_stereo() -> None:
    wav = _wav(channels=2)
    with pytest.raises(ValidationProblemError) as exc:
        validate_wav(wav, **DEFAULTS)
    assert any(e["field"] == "channels" for e in exc.value.errors or [])


def test_rejects_non_pcm() -> None:
    wav = _wav(audio_format=3)  # IEEE float
    with pytest.raises(ValidationProblemError):
        validate_wav(wav, **DEFAULTS)


def _wav(
    *,
    audio_format: int = 1,
    channels: int = 1,
    sample_rate: int = 16_000,
    bits: int = 16,
    data_bytes: int = 320,
) -> bytes:
    byte_rate = sample_rate * channels * bits // 8
    block_align = channels * bits // 8
    payload = bytes(data_bytes)
    fmt_chunk = struct.pack(
        "<4sIHHIIHH",
        b"fmt ",
        16,
        audio_format,
        channels,
        sample_rate,
        byte_rate,
        block_align,
        bits,
    )
    data_chunk = struct.pack("<4sI", b"data", len(payload)) + payload
    riff_size = 4 + len(fmt_chunk) + len(data_chunk)
    return struct.pack("<4sI4s", b"RIFF", riff_size, b"WAVE") + fmt_chunk + data_chunk

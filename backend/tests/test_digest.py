import base64
import hashlib

import pytest

from voice_recorder.digest import (
    compute_content_digest,
    parse_content_digest,
    verify_content_digest,
)
from voice_recorder.problems import BadRequestError, ValidationProblemError


def test_compute_matches_manual() -> None:
    data = b"hello"
    expected = base64.b64encode(hashlib.sha256(data).digest()).decode()
    assert compute_content_digest(data) == f"sha-256=:{expected}:"


def test_parse_canonicalizes() -> None:
    data = b"abc"
    digest = compute_content_digest(data)
    assert parse_content_digest(digest) == digest


def test_missing_header_is_bad_request() -> None:
    with pytest.raises(BadRequestError):
        parse_content_digest(None)


def test_malformed_is_validation_error() -> None:
    with pytest.raises(ValidationProblemError):
        parse_content_digest("md5=:abc:")


def test_bad_base64_is_validation_error() -> None:
    with pytest.raises(ValidationProblemError):
        parse_content_digest("sha-256=:!!!notbase64!!!:")


def test_wrong_length_is_validation_error() -> None:
    short = base64.b64encode(b"tooshort").decode()
    with pytest.raises(ValidationProblemError):
        parse_content_digest(f"sha-256=:{short}:")


def test_verify_roundtrip() -> None:
    data = b"payload"
    assert verify_content_digest(data, compute_content_digest(data))
    assert not verify_content_digest(b"other", compute_content_digest(data))

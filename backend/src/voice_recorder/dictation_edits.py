"""Strict, atomic edits for ephemeral dictation; never repair a model's anchors."""

from __future__ import annotations

import json
import re
import unicodedata
from collections import Counter
from dataclasses import dataclass
from itertools import pairwise
from typing import Any

from .ai.protocols import TerminalRefinementError

MAX_TEXT_CHARS = 4000
MAX_PREVIOUS_CHARS = 8000
MAX_RESPONSE_CHARS = 32768
MAX_EDITS = 64
MAX_EDIT_CHARS = 256
_NUMBERS = re.compile(r"(?<!\w)[+-]?\d+(?:[.,:/-]\d+)*(?:%|\b)")
_TOKENS = re.compile(r"[\w]+(?:[._:/@\\+#-][\w+#]+)*[+#]*", re.UNICODE)
_LINEBREAKS = re.compile(r"[\r\n\v\f\x85\u2028\u2029]")
_NONSPACE_WHITESPACE = re.compile(r"[^\S ]+")
_DUPLICATE_WINDOW = 60


class InvalidDictationEdits(TerminalRefinementError):
    """Only a fixed, content-free category may leave the validator."""

    def __init__(self, category: str) -> None:
        self.category = category
        super().__init__("Invalid dictation edits")


@dataclass(frozen=True)
class ValidatedDictationEdit:
    original: str
    replacement: str


@dataclass(frozen=True)
class ValidatedDictationResult:
    text: str
    edits: tuple[ValidatedDictationEdit, ...]


def _invalid(category: str) -> InvalidDictationEdits:
    return InvalidDictationEdits(category)


def _boundaries(text: str) -> list[tuple[str, str]]:
    return [
        (line[: len(line) - len(line.lstrip())], line[len(line.rstrip()) :])
        for line in _LINEBREAKS.split(text)
    ]


def strict_json_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("Duplicate JSON key")
        result[key] = value
    return result


def _identifiers(text: str) -> Counter[str]:
    # Conservative lexical identifiers: acronyms, camelCase, snake_case, versions,
    # paths, domains, email addresses and words containing digits.
    return Counter(
        token
        for token in _TOKENS.findall(text)
        if (
            any(char.isdigit() for char in token)
            or re.search(r"[a-z][A-Z]", token)
            or (len(token) > 1 and token.isupper())
            or any(char in token for char in "_:/@\\+#")
            or re.search(r"\w[.-]\w", token)
        )
    )


def _letter_scripts(text: str) -> set[str]:
    scripts = set()
    for char in text:
        if unicodedata.category(char).startswith("L"):
            name = unicodedata.name(char, "UNKNOWN")
            # Unicode names identify most scripts by their first word; group
            # CJK ideographs and their compatibility forms as the same script.
            scripts.add("CJK" if "IDEOGRAPH" in name else name.split()[0])
    return scripts


def _windows(text: str) -> Counter[str]:
    return Counter(
        text[index : index + _DUPLICATE_WINDOW]
        for index in range(len(text) - _DUPLICATE_WINDOW + 1)
    )


def apply_dictation_edits(text: str, payload: str, *, previous_text: str = "") -> str:
    return validate_dictation_edits(text, payload, previous_text=previous_text).text


def validate_dictation_edits(
    text: str, payload: str, *, previous_text: str = ""
) -> ValidatedDictationResult:
    if not isinstance(payload, str) or len(payload) > MAX_RESPONSE_CHARS:
        raise _invalid("response_size")
    try:
        document = json.loads(payload, object_pairs_hook=strict_json_object)
    except (ValueError, RecursionError):
        raise _invalid("invalid_json") from None
    if not isinstance(document, dict) or set(document) != {"edits"}:
        raise _invalid("document_shape")
    edits = document["edits"]
    if not isinstance(edits, list) or len(edits) > MAX_EDITS:
        raise _invalid("edit_count")
    spans: list[tuple[int, int, str]] = []
    for edit in edits:
        if not isinstance(edit, list) or len(edit) != 3:
            raise _invalid("edit_shape")
        block, anchor, replacement = edit
        if (
            not all(isinstance(value, str) for value in edit)
            or block != "current"
            or not anchor
            or len(anchor) > MAX_EDIT_CHARS
            or len(replacement) > MAX_EDIT_CHARS
        ):
            raise _invalid("edit_bounds")
        start = text.find(anchor)
        # rfind counts overlapping occurrences too (e.g. "aa" in "aaa").
        if start < 0 or start != text.rfind(anchor):
            raise _invalid("anchor_missing" if start < 0 else "anchor_not_unique")
        if anchor != replacement:
            if anchor.replace(" ", "") == replacement.replace(" ", "") or not anchor.strip():
                raise _invalid("whitespace_only")
            # Line-spanning edits cannot silently move text between paragraphs.
            if _LINEBREAKS.search(anchor) or _LINEBREAKS.search(replacement):
                raise _invalid("line_structure")
            if _NONSPACE_WHITESPACE.findall(anchor) != _NONSPACE_WHITESPACE.findall(replacement):
                raise _invalid("whitespace_structure")
        spans.append((start, start + len(anchor), replacement))
    spans.sort()
    if any(left[1] > right[0] for left, right in pairwise(spans)):
        raise _invalid("overlapping_edits")
    result = text
    for start, end, replacement in reversed(spans):
        result = result[:start] + replacement + result[end:]
    if (
        len(result) > MAX_TEXT_CHARS
        or not result.strip()
        or (any(char.isalnum() for char in text) and not any(c.isalnum() for c in result))
    ):
        raise _invalid("result_bounds")
    if _NUMBERS.findall(text) != _NUMBERS.findall(result):
        raise _invalid("protected_numbers")
    if _identifiers(text) != _identifiers(result):
        raise _invalid("protected_identifiers")
    if not _letter_scripts(result) <= _letter_scripts(text):
        raise _invalid("new_script")
    if _boundaries(text) != _boundaries(result):
        raise _invalid("boundary_whitespace")
    if any(
        unicodedata.category(char) in {"Cc", "Cf", "Cs"} and char not in text for char in result
    ):
        raise _invalid("control_characters")
    original_windows = _windows(text)
    previous_windows = _windows(previous_text)
    for window, count in _windows(result).items():
        if count > 1 and count > original_windows[window]:
            raise _invalid("new_repetition")
        if window in previous_windows and window not in original_windows:
            raise _invalid("context_echo")
    if result == text:
        return ValidatedDictationResult(text=text, edits=())
    return ValidatedDictationResult(
        text=result,
        edits=tuple(
            ValidatedDictationEdit(text[start:end], replacement)
            for start, end, replacement in spans
            if text[start:end] != replacement
        ),
    )

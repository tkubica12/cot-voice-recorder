"""A dependency-free JSON Schema 2020-12 subset validator for OpenAPI 3.1 contract tests.

The project deliberately carries no runtime JSON Schema dependency, so this module
implements exactly the keyword subset that ``openapi/voice-recorder.yaml`` uses. To make
that safe, :func:`assert_keywords_supported` fails loudly the moment the spec introduces
a keyword this validator does not understand — the contract tests can therefore never
silently degrade into checking nothing.
"""

from __future__ import annotations

import re
import uuid
from datetime import datetime
from typing import Any

# Keywords that constrain an instance and are implemented by :func:`validate`.
ASSERTION_KEYWORDS = frozenset(
    {
        "$ref",
        "type",
        "enum",
        "const",
        "format",
        "properties",
        "required",
        "additionalProperties",
        "items",
        "minItems",
        "maxItems",
        "uniqueItems",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "minLength",
        "maxLength",
        "pattern",
        "oneOf",
        "anyOf",
        "allOf",
        "not",
        "nullable",
    }
)

# Keywords that carry documentation/metadata only and are safely ignored.
ANNOTATION_KEYWORDS = frozenset(
    {
        "title",
        "description",
        "default",
        "example",
        "examples",
        "summary",
        "deprecated",
        "readOnly",
        "writeOnly",
        "externalDocs",
        "xml",
        "discriminator",
        "$comment",
        "contentMediaType",
        "contentEncoding",
    }
)

SUPPORTED_KEYWORDS = ASSERTION_KEYWORDS | ANNOTATION_KEYWORDS

_TYPE_CHECKS: dict[str, Any] = {
    "object": lambda v: isinstance(v, dict),
    "array": lambda v: isinstance(v, list),
    "string": lambda v: isinstance(v, str),
    "integer": lambda v: isinstance(v, int) and not isinstance(v, bool),
    "number": lambda v: isinstance(v, int | float) and not isinstance(v, bool),
    "boolean": lambda v: isinstance(v, bool),
    "null": lambda v: v is None,
}


class SchemaResolutionError(AssertionError):
    """A ``$ref`` in the document could not be resolved."""


def resolve_pointer(root: dict[str, Any], ref: str) -> dict[str, Any]:
    """Resolve a local JSON pointer (``#/a/b``). Remote refs are rejected on purpose."""
    if not ref.startswith("#/"):
        raise SchemaResolutionError(f"only local $ref is supported, got {ref!r}")
    node: Any = root
    for raw in ref[2:].split("/"):
        token = raw.replace("~1", "/").replace("~0", "~")
        if not isinstance(node, dict) or token not in node:
            raise SchemaResolutionError(f"unresolvable $ref {ref!r} (missing {token!r})")
        node = node[token]
    if not isinstance(node, dict):
        raise SchemaResolutionError(f"$ref {ref!r} does not point at a schema object")
    return node


def iter_schema_nodes(spec: dict[str, Any]) -> list[tuple[str, dict[str, Any]]]:
    """Return every ``(json-pointer, schema-object)`` reachable in an OpenAPI document."""
    found: list[tuple[str, dict[str, Any]]] = []

    def descend(node: Any, pointer: str) -> None:
        if isinstance(node, dict):
            found.append((pointer, node))
            for key in ("properties", "patternProperties", "$defs"):
                child = node.get(key)
                if isinstance(child, dict):
                    for name, sub in child.items():
                        descend(sub, f"{pointer}/{key}/{name}")
            for key in ("items", "not", "additionalProperties", "contains"):
                child = node.get(key)
                if isinstance(child, dict):
                    descend(child, f"{pointer}/{key}")
            for key in ("oneOf", "anyOf", "allOf", "prefixItems"):
                child = node.get(key)
                if isinstance(child, list):
                    for i, sub in enumerate(child):
                        descend(sub, f"{pointer}/{key}/{i}")

    for name, schema in (spec.get("components", {}).get("schemas") or {}).items():
        descend(schema, f"#/components/schemas/{name}")

    def walk_media(node: Any, pointer: str) -> None:
        if isinstance(node, dict):
            for key, value in node.items():
                if key == "schema" and isinstance(value, dict):
                    descend(value, f"{pointer}/schema")
                else:
                    walk_media(value, f"{pointer}/{key}")
        elif isinstance(node, list):
            for i, value in enumerate(node):
                walk_media(value, f"{pointer}/{i}")

    walk_media(spec.get("paths") or {}, "#/paths")
    walk_media(
        (spec.get("components", {}) or {}).get("parameters") or {}, "#/components/parameters"
    )
    walk_media((spec.get("components", {}) or {}).get("responses") or {}, "#/components/responses")
    return found


def assert_keywords_supported(spec: dict[str, Any]) -> None:
    """Fail if the document uses a schema keyword this validator does not implement."""
    unsupported: set[str] = set()
    for pointer, schema in iter_schema_nodes(spec):
        for keyword in schema:
            if keyword not in SUPPORTED_KEYWORDS:
                unsupported.add(f"{keyword} (at {pointer})")
    if unsupported:
        raise AssertionError(
            "openapi uses schema keywords the contract validator cannot check: "
            + ", ".join(sorted(unsupported))
        )


def assert_refs_resolve(spec: dict[str, Any]) -> None:
    """Fail if any ``$ref`` anywhere in the document is dangling."""
    broken: list[str] = []

    def walk(node: Any, pointer: str) -> None:
        if isinstance(node, dict):
            ref = node.get("$ref")
            if isinstance(ref, str):
                try:
                    resolve_pointer(spec, ref)
                except SchemaResolutionError as exc:
                    broken.append(f"{pointer}: {exc}")
            for key, value in node.items():
                walk(value, f"{pointer}/{key}")
        elif isinstance(node, list):
            for i, value in enumerate(node):
                walk(value, f"{pointer}/{i}")

    walk(spec, "#")
    if broken:
        raise AssertionError("unresolvable $ref(s): " + "; ".join(broken))


def _check_format(value: Any, fmt: str, path: str) -> list[str]:
    if not isinstance(value, str):
        return []
    if fmt == "uuid":
        try:
            uuid.UUID(value)
        except ValueError:
            return [f"{path}: {value!r} is not a valid uuid"]
    elif fmt == "date-time":
        text = value[:-1] + "+00:00" if value.endswith("Z") else value
        try:
            datetime.fromisoformat(text)
        except ValueError:
            return [f"{path}: {value!r} is not a valid RFC 3339 date-time"]
    return []


def validate(
    instance: Any, schema: dict[str, Any], root: dict[str, Any], path: str = "$"
) -> list[str]:
    """Return a list of human-readable validation errors (empty means valid)."""
    errors: list[str] = []

    ref = schema.get("$ref")
    if isinstance(ref, str):
        errors += validate(instance, resolve_pointer(root, ref), root, path)

    declared = schema.get("type")
    if declared is not None:
        types = declared if isinstance(declared, list) else [declared]
        unknown = [t for t in types if t not in _TYPE_CHECKS]
        if unknown:
            raise AssertionError(f"{path}: unknown type(s) {unknown!r} in schema")
        if not any(_TYPE_CHECKS[t](instance) for t in types):
            errors.append(f"{path}: expected type {declared!r}, got {type(instance).__name__}")
            return errors  # further keywords would only produce noise

    if "enum" in schema and instance not in schema["enum"]:
        errors.append(f"{path}: {instance!r} is not one of {schema['enum']!r}")
    if "const" in schema and instance != schema["const"]:
        errors.append(f"{path}: {instance!r} != const {schema['const']!r}")
    if "format" in schema:
        errors += _check_format(instance, str(schema["format"]), path)

    for key, combine in (("allOf", "all"), ("anyOf", "any"), ("oneOf", "one")):
        subschemas = schema.get(key)
        if not isinstance(subschemas, list):
            continue
        results = [validate(instance, sub, root, path) for sub in subschemas]
        passed = [i for i, r in enumerate(results) if not r]
        if combine == "all" and len(passed) != len(results):
            for r in results:
                errors += r
        elif combine == "any" and not passed:
            errors.append(f"{path}: matched none of anyOf ({[e for r in results for e in r]})")
        elif combine == "one" and len(passed) != 1:
            errors.append(
                f"{path}: matched {len(passed)} of oneOf branches, expected exactly 1 "
                f"({[e for r in results for e in r]})"
            )
    if isinstance(schema.get("not"), dict) and not validate(instance, schema["not"], root, path):
        errors.append(f"{path}: must not match the 'not' schema")

    if isinstance(instance, dict):
        errors += _validate_object(instance, schema, root, path)
    elif isinstance(instance, list):
        errors += _validate_array(instance, schema, root, path)
    elif isinstance(instance, str):
        errors += _validate_string(instance, schema, path)
    elif isinstance(instance, int | float) and not isinstance(instance, bool):
        errors += _validate_number(instance, schema, path)

    return errors


def _validate_object(
    instance: dict[str, Any], schema: dict[str, Any], root: dict[str, Any], path: str
) -> list[str]:
    errors: list[str] = []
    properties = schema.get("properties") or {}
    for name in schema.get("required") or []:
        if name not in instance:
            errors.append(f"{path}: missing required property {name!r}")
    for name, value in instance.items():
        sub = properties.get(name)
        if isinstance(sub, dict):
            errors += validate(value, sub, root, f"{path}.{name}")
            continue
        extra = schema.get("additionalProperties")
        if extra is False:
            errors.append(f"{path}: property {name!r} is not allowed (additionalProperties: false)")
        elif isinstance(extra, dict):
            errors += validate(value, extra, root, f"{path}.{name}")
    return errors


def _validate_array(
    instance: list[Any], schema: dict[str, Any], root: dict[str, Any], path: str
) -> list[str]:
    errors: list[str] = []
    items = schema.get("items")
    if isinstance(items, dict):
        for i, value in enumerate(instance):
            errors += validate(value, items, root, f"{path}[{i}]")
    if "minItems" in schema and len(instance) < int(schema["minItems"]):
        errors.append(f"{path}: fewer than minItems={schema['minItems']} items")
    if "maxItems" in schema and len(instance) > int(schema["maxItems"]):
        errors.append(f"{path}: more than maxItems={schema['maxItems']} items")
    if schema.get("uniqueItems") and len(instance) != len({repr(i) for i in instance}):
        errors.append(f"{path}: items are not unique")
    return errors


def _validate_string(instance: str, schema: dict[str, Any], path: str) -> list[str]:
    errors: list[str] = []
    if "minLength" in schema and len(instance) < int(schema["minLength"]):
        errors.append(f"{path}: shorter than minLength={schema['minLength']}")
    if "maxLength" in schema and len(instance) > int(schema["maxLength"]):
        errors.append(f"{path}: longer than maxLength={schema['maxLength']}")
    pattern = schema.get("pattern")
    if isinstance(pattern, str) and re.search(pattern, instance) is None:
        errors.append(f"{path}: does not match pattern {pattern!r}")
    return errors


def _validate_number(instance: float, schema: dict[str, Any], path: str) -> list[str]:
    errors: list[str] = []
    if "minimum" in schema and instance < schema["minimum"]:
        errors.append(f"{path}: {instance} < minimum {schema['minimum']}")
    if "maximum" in schema and instance > schema["maximum"]:
        errors.append(f"{path}: {instance} > maximum {schema['maximum']}")
    if "exclusiveMinimum" in schema and instance <= schema["exclusiveMinimum"]:
        errors.append(f"{path}: {instance} <= exclusiveMinimum {schema['exclusiveMinimum']}")
    if "exclusiveMaximum" in schema and instance >= schema["exclusiveMaximum"]:
        errors.append(f"{path}: {instance} >= exclusiveMaximum {schema['exclusiveMaximum']}")
    return errors

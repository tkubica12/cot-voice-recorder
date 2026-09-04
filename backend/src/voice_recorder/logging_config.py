"""Structured JSON logging without any transcript / audio / token content.

Log records carry a correlation id (``trace_id``) via a context variable so that all
logs produced while handling a request can be joined. We deliberately never log audio
bytes, transcript bodies, or bearer tokens.
"""

from __future__ import annotations

import contextvars
import json
import logging
import sys
from typing import Any

_trace_id: contextvars.ContextVar[str | None] = contextvars.ContextVar("trace_id", default=None)

# Attributes present on every LogRecord; anything else is treated as structured extra.
_STANDARD_ATTRS = frozenset(
    logging.makeLogRecord({}).__dict__.keys() | {"message", "asctime", "taskName"}
)


def set_trace_id(value: str | None) -> None:
    _trace_id.set(value)


def get_trace_id() -> str | None:
    return _trace_id.get()


class JsonFormatter(logging.Formatter):
    """Render log records as single-line JSON."""

    def format(self, record: logging.LogRecord) -> str:
        payload: dict[str, Any] = {
            "level": record.levelname,
            "logger": record.name,
            "message": record.getMessage(),
        }
        trace_id = _trace_id.get()
        if trace_id is not None:
            payload["trace_id"] = trace_id
        for key, value in record.__dict__.items():
            if key not in _STANDARD_ATTRS and not key.startswith("_"):
                payload[key] = value
        if record.exc_info:
            payload["exc_info"] = self.formatException(record.exc_info)
        return json.dumps(payload, ensure_ascii=False, default=str)


def configure_logging(level: str = "INFO") -> None:
    """Install the JSON formatter on the root logger (idempotent)."""
    handler = logging.StreamHandler(sys.stdout)
    handler.setFormatter(JsonFormatter())
    root = logging.getLogger()
    root.handlers.clear()
    root.addHandler(handler)
    root.setLevel(level.upper())
    # Azure SDK http logging is noisy and can leak URLs with SAS tokens; keep it quiet.
    logging.getLogger("azure").setLevel(logging.WARNING)
    logging.getLogger("azure.core.pipeline.policies.http_logging_policy").setLevel(logging.WARNING)
    logging.getLogger("uvicorn.access").setLevel(logging.WARNING)


def get_logger(name: str) -> logging.Logger:
    return logging.getLogger(name)

"""Console entrypoints: API server, queue worker, and cleanup job.

The same image runs any of these via ``voice-recorder <command>``.
"""

from __future__ import annotations

import argparse
import json
import sys

from .config import get_settings
from .logging_config import configure_logging, get_logger

logger = get_logger(__name__)


def _run_api(args: argparse.Namespace) -> int:
    import uvicorn

    uvicorn.run(
        "voice_recorder.app:_factory",
        factory=True,
        host=args.host,
        port=args.port,
        log_config=None,
    )
    return 0


def _run_worker(_args: argparse.Namespace) -> int:
    from .bootstrap import build_context
    from .worker import run_worker

    settings = get_settings()
    configure_logging(settings.log_level)
    run_worker(build_context(settings))
    return 0


def _run_cleanup(_args: argparse.Namespace) -> int:
    from .bootstrap import build_context
    from .services.cleanup import run_cleanup

    settings = get_settings()
    configure_logging(settings.log_level)
    result = run_cleanup(build_context(settings))
    sys.stdout.write(json.dumps(result.as_dict()) + "\n")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="voice-recorder")
    sub = parser.add_subparsers(dest="command", required=True)

    api = sub.add_parser("api", help="Run the FastAPI server.")
    api.add_argument("--host", default="0.0.0.0")
    api.add_argument("--port", type=int, default=8000)
    api.set_defaults(func=_run_api)

    worker = sub.add_parser("worker", help="Run the queue worker.")
    worker.set_defaults(func=_run_worker)

    cleanup = sub.add_parser("cleanup", help="Run the retention cleanup job.")
    cleanup.set_defaults(func=_run_cleanup)

    args = parser.parse_args(argv)
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())

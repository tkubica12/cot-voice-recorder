"""FastAPI application factory: middleware, exception handlers, and wiring."""

from __future__ import annotations

import uuid
from collections.abc import AsyncIterator, Awaitable, Callable
from contextlib import asynccontextmanager

from fastapi import FastAPI, Request
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse, Response
from starlette.exceptions import HTTPException as StarletteHTTPException

from .auth import TokenVerifier
from .config import Settings, get_settings
from .logging_config import configure_logging, get_logger, set_trace_id
from .problems import (
    ProblemError,
    ValidationProblemError,
)
from .routers import health, realtime, recordings, transcripts
from .services.context import ServiceContext

logger = get_logger(__name__)

PROBLEM_MEDIA_TYPE = "application/problem+json"

_HTTP_TITLES = {
    404: ("not-found", "Not found"),
    405: ("method-not-allowed", "Method not allowed"),
    406: ("not-acceptable", "Not acceptable"),
}


def _problem_response(request: Request, exc: ProblemError) -> JSONResponse:
    from .logging_config import get_trace_id

    body = exc.to_problem(instance=request.url.path, trace_id=get_trace_id())
    return JSONResponse(
        status_code=exc.status,
        content=body,
        media_type=PROBLEM_MEDIA_TYPE,
        headers=exc.headers or None,
    )


def create_app(
    *,
    settings: Settings | None = None,
    context: ServiceContext | None = None,
    verifier: TokenVerifier | None = None,
) -> FastAPI:
    settings = settings or (context.settings if context else get_settings())
    configure_logging(settings.log_level)

    @asynccontextmanager
    async def lifespan(app: FastAPI) -> AsyncIterator[None]:
        if context is not None and verifier is not None:
            app.state.context = context
            app.state.verifier = verifier
        else:
            from .bootstrap import build_context, build_token_verifier

            app.state.context = build_context(settings)
            app.state.verifier = build_token_verifier(settings)
        app.state.settings = settings
        app.state.ready = True
        logger.info("startup_complete", extra={"environment": settings.environment})
        try:
            yield
        finally:
            app.state.ready = False
            logger.info("shutdown_complete")

    app = FastAPI(
        title="Voice Recorder API",
        version="1.0.0",
        lifespan=lifespan,
    )

    @app.middleware("http")
    async def trace_id_middleware(
        request: Request, call_next: Callable[[Request], Awaitable[Response]]
    ) -> Response:
        trace_id = request.headers.get("X-Request-Id") or str(uuid.uuid4())
        set_trace_id(trace_id)
        try:
            response = await call_next(request)
        finally:
            pass
        response.headers["X-Request-Id"] = trace_id
        return response

    @app.exception_handler(ProblemError)
    async def _handle_problem(request: Request, exc: ProblemError) -> JSONResponse:
        if exc.status >= 500:
            logger.error("request_error", extra={"code": exc.code, "status": exc.status})
        return _problem_response(request, exc)

    @app.exception_handler(RequestValidationError)
    async def _handle_validation(request: Request, exc: RequestValidationError) -> JSONResponse:
        errors = [
            {
                "field": ".".join(str(p) for p in err.get("loc", []) if p != "body"),
                "message": err.get("msg", "invalid"),
            }
            for err in exc.errors()
        ]
        problem = ValidationProblemError("One or more fields are invalid.", errors=errors)
        return _problem_response(request, problem)

    @app.exception_handler(StarletteHTTPException)
    async def _handle_http(request: Request, exc: StarletteHTTPException) -> JSONResponse:
        code, title = _HTTP_TITLES.get(exc.status_code, ("http-error", "HTTP error"))
        problem = ProblemError(str(exc.detail))
        problem.status = exc.status_code
        problem.code = code
        problem.title = title
        headers = dict(exc.headers or {})
        problem.headers = headers
        return _problem_response(request, problem)

    @app.exception_handler(Exception)
    async def _handle_unexpected(request: Request, exc: Exception) -> JSONResponse:
        logger.exception("unhandled_error")
        problem = ProblemError("An unexpected error occurred.")
        return _problem_response(request, problem)

    app.include_router(health.router)
    app.include_router(recordings.router)
    app.include_router(transcripts.router)
    app.include_router(realtime.router)
    return app


# ASGI entrypoint for `uvicorn voice_recorder.app:app`.
def _factory() -> FastAPI:
    return create_app()


__all__ = ["create_app"]

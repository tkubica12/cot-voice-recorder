"""Unauthenticated health probes."""

from __future__ import annotations

from fastapi import APIRouter, Request, Response

from ..models import HealthStatus

router = APIRouter(tags=["health"])


@router.get("/health/live", response_model=HealthStatus, operation_id="getLiveness")
def get_liveness() -> HealthStatus:
    return HealthStatus(status="ok")


@router.get("/health/ready", response_model=HealthStatus, operation_id="getReadiness")
def get_readiness(request: Request, response: Response) -> HealthStatus:
    # Readiness must not block startup on Foundry; we only require the composition
    # root to be wired. Dependency health is reported without failing the process.
    ready = bool(getattr(request.app.state, "ready", False))
    if not ready:
        response.status_code = 503
        return HealthStatus(status="not_ready")
    return HealthStatus(status="ok")

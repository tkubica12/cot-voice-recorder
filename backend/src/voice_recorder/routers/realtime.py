"""Web PubSub negotiation route."""

from __future__ import annotations

from fastapi import APIRouter, Depends

from ..auth import AuthenticatedUser
from ..deps import get_context, require_user
from ..models import NegotiateRequest, NegotiateResponse
from ..services.context import ServiceContext

router = APIRouter(prefix="/v1/realtime", tags=["realtime"])


@router.post("/negotiate", response_model=NegotiateResponse, operation_id="negotiateRealtime")
def negotiate_route(
    _body: NegotiateRequest | None = None,
    _user: AuthenticatedUser = Depends(require_user),
    ctx: ServiceContext = Depends(get_context),
) -> NegotiateResponse:
    access = ctx.realtime.negotiate(ctx.user_id)
    return NegotiateResponse(
        url=access.url,
        hub=access.hub,
        group=access.group,
        expires_at=access.expires_at,
    )

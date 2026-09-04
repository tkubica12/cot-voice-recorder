"""Composition root: build a :class:`ServiceContext` and a token verifier from settings.

Production uses managed identity (:class:`DefaultAzureCredential`) for every Azure
data-plane client. A gated local/test mode (never in production) may point Storage at
Azurite via its development connection string and swap AI/realtime for in-memory fakes.
"""

from __future__ import annotations

from typing import Any

from .ai.fakes import FakeRefiner, FakeTranscriber
from .ai.foundry import FoundryRefiner, FoundryTranscriber
from .auth import GoogleTokenVerifier, StaticTokenVerifier, TokenVerifier
from .clock import Clock, SystemClock
from .config import Settings
from .realtime.memory import InMemoryRealtimeGateway
from .realtime.protocols import RealtimeGateway
from .repositories.tables import (
    AzureTableChunkRepository,
    AzureTableRecordingRepository,
    AzureTableTranscriptRepository,
)
from .services.context import ServiceContext
from .storage.blob import AzureBlobStore
from .storage.queue import AzureWorkQueue, build_queue_client


def _credential() -> Any:
    from azure.identity import DefaultAzureCredential

    return DefaultAzureCredential()


def build_token_verifier(settings: Settings) -> TokenVerifier:
    if settings.auth_disabled:
        if settings.environment == "production":  # pragma: no cover - guarded by config
            raise RuntimeError("auth cannot be disabled in production")
        # Local/dev convenience only. Accepts a single dev token.
        from .auth import AuthenticatedUser

        return StaticTokenVerifier(
            valid_tokens={
                "dev-token": AuthenticatedUser(
                    email=settings.allowlisted_email or "dev@example.com",
                    subject="dev",
                    audience="dev",
                )
            }
        )
    return GoogleTokenVerifier(
        jwks_uri=settings.google_jwks_uri,
        allowed_audiences=settings.google_allowed_audiences,
        allowed_issuers=settings.google_allowed_issuers,
        allowlisted_email=settings.allowlisted_email,
    )


def _build_blob_store(settings: Settings, credential: Any) -> AzureBlobStore:
    from azure.storage.blob import BlobServiceClient

    containers = [
        settings.audio_container,
        settings.transcript_container,
        settings.raw_container,
    ]
    if settings.storage_use_azurite:
        service = BlobServiceClient.from_connection_string(
            settings.storage_azurite_connection_string
        )
    else:
        service = BlobServiceClient(settings.blob_endpoint, credential=credential)
    return AzureBlobStore(service, containers)


def _build_queue(settings: Settings, credential: Any) -> AzureWorkQueue:
    client = build_queue_client(
        use_azurite=settings.storage_use_azurite,
        connection_string=settings.storage_azurite_connection_string,
        queue_endpoint=settings.queue_endpoint,
        queue_name=settings.work_queue,
        credential=credential,
    )
    return AzureWorkQueue(client)


def _build_table(settings: Settings, credential: Any, table_name: str) -> Any:
    from azure.data.tables import TableServiceClient

    if settings.storage_use_azurite:
        service = TableServiceClient.from_connection_string(
            settings.storage_azurite_connection_string
        )
    else:
        service = TableServiceClient(endpoint=settings.table_endpoint, credential=credential)
    return service.get_table_client(table_name)


def _build_ai(settings: Settings, credential: Any) -> tuple[Any, Any]:
    if settings.use_fake_ai:
        return FakeTranscriber(), FakeRefiner()
    from azure.identity import get_bearer_token_provider
    from openai import AzureOpenAI

    token_provider = get_bearer_token_provider(credential, settings.foundry_scope)
    client = AzureOpenAI(
        azure_endpoint=settings.foundry_endpoint,
        azure_ad_token_provider=token_provider,
        api_version=settings.foundry_api_version,
    )
    return (
        FoundryTranscriber(client, settings.transcribe_deployment),
        FoundryRefiner(client),
    )


def _build_realtime(settings: Settings, credential: Any) -> RealtimeGateway:
    if settings.use_fake_realtime or not settings.webpubsub_endpoint:
        return InMemoryRealtimeGateway(hub=settings.webpubsub_hub, group=settings.webpubsub_group)
    from azure.messaging.webpubsubservice import WebPubSubServiceClient

    from .realtime.webpubsub import WebPubSubGateway

    client = WebPubSubServiceClient(
        endpoint=settings.webpubsub_endpoint,
        hub=settings.webpubsub_hub,
        credential=credential,
    )
    return WebPubSubGateway(
        client,
        hub=settings.webpubsub_hub,
        group=settings.webpubsub_group,
        token_ttl_minutes=settings.webpubsub_token_ttl_minutes,
        clock=SystemClock(),
    )


def build_context(settings: Settings, *, clock: Clock | None = None) -> ServiceContext:
    clock = clock or SystemClock()
    credential = _credential() if settings.uses_managed_identity else None
    transcriber, refiner = _build_ai(settings, credential)
    realtime = _build_realtime(settings, credential)

    if settings.use_fake_storage:
        from .repositories.memory import (
            InMemoryChunkRepository,
            InMemoryRecordingRepository,
            InMemoryTranscriptRepository,
        )
        from .storage.memory import InMemoryBlobStore, InMemoryWorkQueue

        return ServiceContext(
            settings=settings,
            clock=clock,
            recordings=InMemoryRecordingRepository(),
            chunks=InMemoryChunkRepository(),
            transcripts=InMemoryTranscriptRepository(),
            blobs=InMemoryBlobStore(),
            queue=InMemoryWorkQueue(),
            transcriber=transcriber,
            refiner=refiner,
            realtime=realtime,
            user_id=settings.webpubsub_group,
        )

    blobs = _build_blob_store(settings, credential)
    queue = _build_queue(settings, credential)
    recordings = AzureTableRecordingRepository(
        _build_table(settings, credential, settings.recordings_table)
    )
    chunks = AzureTableChunkRepository(_build_table(settings, credential, settings.chunks_table))
    transcripts = AzureTableTranscriptRepository(
        _build_table(settings, credential, settings.transcripts_table)
    )
    return ServiceContext(
        settings=settings,
        clock=clock,
        recordings=recordings,
        chunks=chunks,
        transcripts=transcripts,
        blobs=blobs,
        queue=queue,
        transcriber=transcriber,
        refiner=refiner,
        realtime=realtime,
        user_id=settings.webpubsub_group,
    )

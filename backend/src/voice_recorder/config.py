"""Application configuration.

Settings are loaded from environment variables (and an optional ``.env`` file for
local development). Production Azure data-plane access uses managed identity only;
account keys / connection strings are accepted **only** in a clearly gated local test
mode (``environment != "production"`` together with ``storage_use_azurite=true``).
"""

from __future__ import annotations

from functools import lru_cache
from typing import Annotated, Literal

from pydantic import field_validator, model_validator
from pydantic_settings import BaseSettings, NoDecode, SettingsConfigDict

Environment = Literal["production", "local", "test"]
TranscribeProvider = Literal["azure_openai", "azure_speech"]
TranscribeStyle = Literal["verbatim", "clean"]

# Development connection string documented by Azurite. Only usable outside production.
AZURITE_DEV_CONNECTION_STRING = (
    "DefaultEndpointsProtocol=http;AccountName=devstoreaccount1;"
    "AccountKey=Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/"
    "K1SZFPTOtr/KBHBeksoGMGw==;"
    "BlobEndpoint=http://127.0.0.1:10000/devstoreaccount1;"
    "QueueEndpoint=http://127.0.0.1:10001/devstoreaccount1;"
    "TableEndpoint=http://127.0.0.1:10002/devstoreaccount1;"
)

DEFAULT_GLOSSARY_TERMS = (
    "Microsoft, Azure, GitHub, SDK, API, AI, Container Apps, Managed Identity, "
    "Foundry, Copilot, Backend, Speech-to-text, MAI-Transcribe-2, GPT-5.6 Luna, "
    "REST API, Blob Storage, Kubernetes, Entra, OpenAI, FastAPI, Python, DevOps, "
    "Terraform, Azure OpenAI, Web PubSub, DNS, OIDC, JWT"
)


class Settings(BaseSettings):
    """Typed application settings."""

    model_config = SettingsConfigDict(
        env_prefix="VR_",
        env_file=".env",
        env_file_encoding="utf-8",
        extra="ignore",
    )

    environment: Environment = "production"
    log_level: str = "INFO"

    # ------------------------------------------------------------------ auth
    # NoDecode: env values are passed as-is to the `_split_csv` validator below
    # instead of being JSON-decoded by pydantic-settings, so comma-separated
    # values (e.g. VR_GOOGLE_ALLOWED_AUDIENCES=a,b) load correctly from the env.
    google_allowed_audiences: Annotated[tuple[str, ...], NoDecode] = ()
    google_allowed_issuers: Annotated[tuple[str, ...], NoDecode] = (
        "https://accounts.google.com",
        "accounts.google.com",
    )
    google_jwks_uri: str = "https://www.googleapis.com/oauth2/v3/certs"
    allowlisted_email: str = ""
    # In tests we swap the verifier for a fake; never enable in production.
    auth_disabled: bool = False

    # --------------------------------------------------------------- storage
    storage_account_name: str = ""
    storage_use_azurite: bool = False
    storage_azurite_connection_string: str = AZURITE_DEV_CONNECTION_STRING
    audio_container: str = "audio"
    transcript_container: str = "transcripts"
    raw_container: str = "raw"
    work_queue: str = "work"
    recordings_table: str = "recordings"
    chunks_table: str = "chunks"
    transcripts_table: str = "transcripts"

    # ---------------------------------------------------------------- foundry
    foundry_endpoint: str = "https://tomaskubica-foundry-resource.cognitiveservices.azure.com/"
    foundry_api_version: str = "2024-10-21"
    foundry_scope: str = "https://cognitiveservices.azure.com/.default"
    transcribe_provider: TranscribeProvider = "azure_speech"
    transcribe_deployment: str = "gpt-4o-transcribe"
    speech_endpoint: str = ""
    speech_api_version: str = "2025-10-15"
    speech_model: str = "MAI-Transcribe-2"
    speech_transcribe_style: TranscribeStyle = "verbatim"
    speech_timeout_seconds: float = 180.0
    refine_deployment_default: str = "gpt-5.6-luna"
    refine_deployment_alternative: str = "gpt-5.6-terra"

    # ------------------------------------------------------------- web pubsub
    webpubsub_endpoint: str = ""
    webpubsub_hub: str = "transcripts"
    webpubsub_group: str = "user"
    webpubsub_token_ttl_minutes: int = 60

    # ------------------------------------------------- local/test fallbacks
    # Gated (non-production only): use deterministic in-memory AI / realtime / storage
    # so the service can run without Foundry, Web PubSub, or Azure Storage access
    # during local smoke tests.
    use_fake_ai: bool = False
    use_fake_realtime: bool = False
    use_fake_storage: bool = False

    # ------------------------------------------------------------- behaviour
    transcribe_language: str = "cs"
    glossary_terms: str = DEFAULT_GLOSSARY_TERMS
    max_chunk_bytes: int = 8 * 1024 * 1024
    wav_sample_rate: int = 16_000
    wav_channels: int = 1
    wav_bits_per_sample: int = 16
    overlap_min_chars: int = 8
    overlap_max_chars: int = 400
    retention_hours: int = 48
    chunk_grace_seconds: int = 900
    max_transcribe_attempts: int = 5
    max_refine_attempts: int = 5
    # Processing lease for a dequeued message. It must comfortably exceed the slowest
    # Foundry transcription/refinement call, otherwise the message becomes visible again
    # mid-processing and a second worker issues a duplicate model call.
    queue_visibility_seconds: int = 300
    # Number of messages pulled per receive. Kept at 1: messages are processed
    # sequentially, so a larger batch would let the visibility lease of the last message
    # expire while earlier ones are still being transcribed.
    queue_batch_size: int = 1
    # Base for the exponential retry backoff applied to a failed message. Deliberately
    # smaller than the visibility lease: the lease sizes one attempt, this sizes the wait
    # between attempts.
    queue_retry_base_seconds: int = 30
    # Wait applied after a queue *receive* failure before polling again (doubles up to
    # queue_receive_error_max_backoff_seconds while the failure persists).
    queue_receive_error_backoff_seconds: float = 2.0
    queue_receive_error_max_backoff_seconds: float = 60.0
    preview_max_chars: int = 140

    # ------------------------------------------------------------ validation
    @field_validator("google_allowed_audiences", "google_allowed_issuers", mode="before")
    @classmethod
    def _split_csv(cls, value: object) -> object:
        """Allow comma-separated strings for tuple settings from the environment."""
        if isinstance(value, str):
            return tuple(item.strip() for item in value.split(",") if item.strip())
        return value

    @model_validator(mode="after")
    def _validate(self) -> Settings:
        if self.queue_batch_size < 1:
            raise ValueError("queue_batch_size must be >= 1")
        if self.environment == "production":
            if self.storage_use_azurite:
                raise ValueError("storage_use_azurite must be false in production")
            if not self.storage_account_name:
                raise ValueError("storage_account_name is required in production")
            if not self.auth_disabled and (
                not self.google_allowed_audiences or not self.allowlisted_email
            ):
                raise ValueError(
                    "google_allowed_audiences and allowlisted_email are required in production"
                )
            if self.auth_disabled:
                raise ValueError("auth_disabled must never be true in production")
            if self.use_fake_ai or self.use_fake_realtime or self.use_fake_storage:
                raise ValueError("fake AI/realtime/storage adapters are not allowed in production")
            if not self.webpubsub_endpoint:
                raise ValueError("webpubsub_endpoint is required in production")
            if self.transcribe_provider == "azure_speech" and not self.speech_endpoint:
                raise ValueError(
                    "speech_endpoint is required for the azure_speech transcribe provider"
                )
        return self

    # -------------------------------------------------------------- helpers
    @property
    def uses_managed_identity(self) -> bool:
        """True when Azure data-plane access uses managed identity (not Azurite)."""
        return not self.storage_use_azurite

    @property
    def blob_endpoint(self) -> str:
        return f"https://{self.storage_account_name}.blob.core.windows.net"

    @property
    def queue_endpoint(self) -> str:
        return f"https://{self.storage_account_name}.queue.core.windows.net"

    @property
    def table_endpoint(self) -> str:
        return f"https://{self.storage_account_name}.table.core.windows.net"

    def glossary_prompt(self) -> str:
        return f"Domain glossary (spell these technical terms correctly): {self.glossary_terms}."

    def glossary_phrases(self) -> tuple[str, ...]:
        return tuple(term.strip() for term in self.glossary_terms.split(",") if term.strip())


@lru_cache(maxsize=1)
def get_settings() -> Settings:
    """Return the process-wide settings singleton."""
    return Settings()

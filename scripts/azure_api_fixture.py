"""Owned HTTPS Azurite API contexts; never real Azure, ambient credentials or paid AI."""

from __future__ import annotations

import json
import re
import tempfile
import threading
import time
import uuid
from contextlib import ExitStack
from pathlib import Path
from typing import TYPE_CHECKING
from urllib.parse import urlsplit

import httpx
from azure.core.credentials import AccessToken
from azure.core.pipeline.transport import RequestsTransport
from azure.data.tables import TableServiceClient
from azure.storage.blob import BlobServiceClient
from azure.storage.queue import QueueClient, TextBase64DecodePolicy, TextBase64EncodePolicy

if TYPE_CHECKING:
    from measure_backend import Authority, Fixture
    from voice_recorder.config import Settings
    from voice_recorder.realtime.protocols import RealtimeGateway
    from voice_recorder.services.context import ServiceContext

SCOPE = "authenticated-https-azurite-http-no-ai-realtime"
FIELDS = {"scope", "key", "blob", "queue", "table", "identity_endpoint", "identity_header"}


def load_fixture(root: Path) -> dict[str, str]:
    if not root.is_absolute():
        raise ValueError("An explicit absolute owned Azure fixture root is required.")
    path = root.joinpath("fixture.json")
    if path.stat().st_size > 8192:
        raise ValueError("Azure fixture configuration exceeds its bound.")
    config = json.loads(path.read_bytes())
    if (
        not isinstance(config, dict)
        or set(config) != FIELDS
        or any(type(value) is not str for value in config.values())
        or config["scope"] != SCOPE
        or not re.fullmatch(r"[a-f0-9]{32}", config["key"])
        or not re.fullmatch(r"[a-f0-9]{32}", config["identity_header"])
    ):
        raise ValueError("Invalid owned Azure API fixture marker.")
    for kind in ["blob", "queue", "table", "identity_endpoint"]:
        value = urlsplit(config[kind])
        identity = kind == "identity_endpoint"
        if (
            value.scheme != ("http" if identity else "https")
            or value.hostname != "127.0.0.1"
            or not value.port
            or value.username
            or value.password
            or value.query
            or value.fragment
            or value.path != ("/token" if identity else "/devstoreaccount1/")
        ):
            raise ValueError("Only explicit local identity and verified-HTTPS Azurite are allowed.")
    if root.joinpath("server-cert.pem").stat().st_size > 65536:
        raise ValueError("Explicit Azure fixture certificate exceeds its bound.")
    return config


def resource_names(key: str) -> dict[str, str]:
    if not re.fullmatch(r"[a-f0-9]{32}", key):
        raise ValueError("An owned resource namespace is required.")
    return {
        "audio_container": f"audio-{key}",
        "transcript_container": f"transcripts-{key}",
        "raw_container": f"raw-{key}",
        "work_queue": f"work-{key}",
        "recordings_table": f"Recordings{key}",
        "chunks_table": f"Chunks{key}",
        "transcripts_table": f"Transcripts{key}",
    }


class LocalIdentity:
    def __init__(self, config: dict[str, str]) -> None:
        self.config = config
        self.http = httpx.Client(trust_env=False, timeout=10)
        self.lock = threading.Lock()
        self.cached: AccessToken | None = None

    def get_token(self, *scopes: str, **kwargs: object) -> AccessToken:
        if scopes != ("https://storage.azure.com/.default",):
            raise ValueError("Only the storage data-plane scope is allowed.")
        with self.lock:
            if self.cached and self.cached.expires_on > time.time() + 300:
                return self.cached
            response = self.http.get(
                self.config["identity_endpoint"],
                headers={"X-IDENTITY-HEADER": self.config["identity_header"]},
                params={
                    "api-version": "2019-08-01",
                    "client_id": "synthetic-uami",
                    "resource": "https://storage.azure.com/",
                },
            )
            response.raise_for_status()
            payload = response.json()
            expiry = int(payload["expires_on"])
            if not payload["access_token"] or expiry <= time.time() + 300:
                raise ValueError("Synthetic identity did not return a usable bounded token.")
            self.cached = AccessToken(payload["access_token"], expiry)
            return self.cached

    def close(self) -> None:
        self.http.close()


def context(
    root: Path, settings: Settings, realtime: RealtimeGateway, resources: ExitStack
) -> ServiceContext:
    from voice_recorder.ai.fakes import FakeRefiner, FakeTranscriber
    from voice_recorder.clock import SystemClock
    from voice_recorder.repositories.tables import (
        AzureTableChunkRepository,
        AzureTableRecordingRepository,
        AzureTableTranscriptRepository,
    )
    from voice_recorder.services.context import ServiceContext
    from voice_recorder.storage.blob import AzureBlobStore
    from voice_recorder.storage.queue import AzureWorkQueue

    config = load_fixture(root)
    names = resource_names(config["key"])
    settings = settings.model_copy(update={**names, "use_fake_storage": False})
    credential = LocalIdentity(config)
    resources.callback(credential.close)
    transport = RequestsTransport(
        connection_verify=str(root.joinpath("server-cert.pem")),
        use_env_settings=False,
        connection_timeout=10,
        read_timeout=60,
    )
    resources.callback(transport.close)
    blob = BlobServiceClient(
        config["blob"].rstrip("/"),
        credential=credential,
        transport=transport,
        api_version="2023-11-03",
    )
    resources.callback(blob.close)
    table = TableServiceClient(
        config["table"].rstrip("/"), credential=credential, transport=transport
    )
    resources.callback(table.close)
    queue = QueueClient(
        config["queue"].rstrip("/"),
        names["work_queue"],
        credential=credential,
        transport=transport,
        api_version="2023-11-03",
        message_encode_policy=TextBase64EncodePolicy(),
        message_decode_policy=TextBase64DecodePolicy(),
    )
    resources.callback(queue.close)
    return ServiceContext(
        settings=settings,
        clock=SystemClock(),
        recordings=AzureTableRecordingRepository(table.get_table_client(names["recordings_table"])),
        chunks=AzureTableChunkRepository(table.get_table_client(names["chunks_table"])),
        transcripts=AzureTableTranscriptRepository(
            table.get_table_client(names["transcripts_table"])
        ),
        blobs=AzureBlobStore(
            blob,
            [names[key] for key in ["audio_container", "transcript_container", "raw_container"]],
        ),
        queue=AzureWorkQueue(queue),
        transcriber=FakeTranscriber(),
        refiner=FakeRefiner(),
        realtime=realtime,
    )


class AzureProfile:
    def __init__(
        self,
        root: Path,
        endpoints: dict,
        identity: object,
        blob: object,
        queue: object,
        table: object,
    ) -> None:
        self.root, self.endpoints, self.identity = root, endpoints, identity
        self.blob, self.queue, self.table = blob, queue, table
        self.observations: list[dict] = []

    def observe(self, names: dict[str, str]) -> dict[str, int]:
        recordings = list(self.table.get_table_client(names["recordings_table"]).list_entities())
        recordings = [row for row in recordings if row["PartitionKey"] == "recording"]
        chunks = list(self.table.get_table_client(names["chunks_table"]).list_entities())
        transcripts = list(self.table.get_table_client(names["transcripts_table"]).list_entities())
        for row in recordings:
            if row["state"] != "transcribing":
                raise ValueError(
                    "Measured API did not retain the actual incomplete recording state."
                )
        if len(chunks) != len(recordings) or len(transcripts) != len(recordings):
            raise ValueError("Actual SDK cannot reopen the complete measured Table workload.")
        for row in transcripts:
            body = (
                self.blob.get_blob_client(names["transcript_container"], row["body_path"])
                .download_blob()
                .readall()
            )
            envelope = json.loads(body)
            if (
                envelope.get("text") != "synthetic final"
                or envelope.get("raw_text") != "synthetic original"
            ):
                raise ValueError("Actual SDK could not reopen the measured transcript Blob.")
        messages = self.queue.get_queue_client(names["work_queue"]).get_queue_properties()
        return {
            "recordings": len(recordings),
            "chunks": len(chunks),
            "transcripts": len(transcripts),
            "queued_messages": messages.approximate_message_count,
        }

    def fixture(self, implementation: str, rust: Path, authority: Authority) -> Fixture:
        from measure_backend import Fixture

        resources = ExitStack()
        try:
            root = Path(
                resources.enter_context(
                    tempfile.TemporaryDirectory(prefix="api-profile-", dir=self.root)
                )
            )
            key = uuid.uuid4().hex
            config = {
                "scope": SCOPE,
                "key": key,
                **self.endpoints,
                "identity_endpoint": self.identity.endpoint,
                "identity_header": self.identity.header,
            }
            root.joinpath("fixture.json").write_text(json.dumps(config))
            root.joinpath("fixture.json").chmod(0o600)
            root.joinpath("server-cert.pem").write_bytes(
                self.root.joinpath("server-cert.pem").read_bytes()
            )
            load_fixture(root)
            names = resource_names(key)
            for kind in ["audio_container", "transcript_container", "raw_container"]:
                self.blob.create_container(names[kind])
                resources.callback(self.blob.delete_container, names[kind])
            self.queue.create_queue(names["work_queue"])
            resources.callback(self.queue.delete_queue, names["work_queue"])
            for kind in ["recordings_table", "chunks_table", "transcripts_table"]:
                self.table.create_table(names[kind])
                resources.callback(self.table.delete_table, names[kind])

            class OwnedFixture(Fixture):
                def close(self) -> None:
                    try:
                        super().close()
                        counts = profile.observe(names)
                        observation = {
                            "implementation": implementation,
                            "pid": self.process.pid,
                            "storage_backend": self.storage_backend,
                            "key": key,
                            **counts,
                        }
                    finally:
                        resources.close()
                    observation["resources_deleted"] = True
                    profile.observations.append(observation)

            profile = self
            return OwnedFixture(implementation, rust, authority, azure_root=root)
        except BaseException:
            resources.close()
            raise

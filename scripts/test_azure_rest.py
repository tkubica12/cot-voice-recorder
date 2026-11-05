"""Actual Rust REST versus pinned local HTTPS Azurite and Azure SDK; never Azure IAM proof."""

from __future__ import annotations

import argparse
import base64
import hashlib
import ipaddress
import json
import os
import re
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from contextlib import suppress
from datetime import UTC, datetime, timedelta
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

import jwt
from azure.core.exceptions import ResourceExistsError, ServiceRequestError
from azure.core.pipeline.transport import RequestsTransport
from azure.data.tables import EdmType, EntityProperty, TableServiceClient
from azure.storage.blob import BlobServiceClient
from azure.storage.queue import QueueServiceClient
from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID
from measure_synthetic_capture import create

ROOT = Path(__file__).resolve().parents[1]
SCOPE = "https-azurite-production-rest-contract"
IMAGE = (
    "mcr.microsoft.com/azure-storage/azurite@"
    "sha256:dae2a5f96553962901304b94e72ef87e299d0825e4b679673bcc527a25076fe4"
)
TEST = "storage::azurite_tests::https_azure_contract"
AZURITE_DEV_CONNECTION_STRING = (
    "DefaultEndpointsProtocol=http;AccountName=devstoreaccount1;"
    "AccountKey=Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/"
    "K1SZFPTOtr/KBHBeksoGMGw==;"
    "BlobEndpoint=http://127.0.0.1:10000/devstoreaccount1;"
    "QueueEndpoint=http://127.0.0.1:10001/devstoreaccount1;"
    "TableEndpoint=http://127.0.0.1:10002/devstoreaccount1;"
)


def certificate(root: Path) -> rsa.RSAPrivateKey:
    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "VoicePrompt synthetic Azurite")])
    now = datetime.now(UTC)
    cert = (
        x509.CertificateBuilder()
        .subject_name(name)
        .issuer_name(name)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - timedelta(minutes=1))
        .not_valid_after(now + timedelta(hours=2))
        .add_extension(x509.BasicConstraints(ca=True, path_length=0), critical=True)
        .add_extension(
            x509.SubjectAlternativeName(
                [
                    x509.IPAddress(ipaddress.ip_address("127.0.0.1")),
                    x509.DNSName("localhost"),
                ]
            ),
            critical=False,
        )
        .add_extension(x509.ExtendedKeyUsage([ExtendedKeyUsageOID.SERVER_AUTH]), critical=False)
        .sign(key, hashes.SHA256())
    )
    root.joinpath("server-cert.pem").write_bytes(cert.public_bytes(serialization.Encoding.PEM))
    path = root.joinpath("server-key.pem")
    path.write_bytes(
        key.private_bytes(
            serialization.Encoding.PEM,
            serialization.PrivateFormat.PKCS8,
            serialization.NoEncryption(),
        )
    )
    path.chmod(0o600)
    return key


class Identity:
    def __init__(self, key: rsa.RSAPrivateKey) -> None:
        self.header = uuid.uuid4().hex
        self.resources: list[str] = []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self) -> None:
                parsed = urlsplit(self.path)
                query = parse_qs(parsed.query)
                if (
                    parsed.path != "/token"
                    or query.get("api-version") != ["2019-08-01"]
                    or query.get("client_id") != ["synthetic-uami"]
                    or query.get("resource") != ["https://storage.azure.com/"]
                    or self.headers.get("X-IDENTITY-HEADER") != owner.header
                ):
                    self.send_error(403)
                    return
                owner.resources.append(query["resource"][0])
                now = int(time.time())
                token = jwt.encode(
                    {
                        "iss": "https://sts.windows.net/00000000-0000-0000-0000-000000000001/",
                        "aud": "https://storage.azure.com",
                        "iat": now - 1,
                        "nbf": now - 1,
                        "exp": now + 3600,
                        "tid": "00000000-0000-0000-0000-000000000001",
                    },
                    key,
                    algorithm="RS256",
                )
                body = json.dumps({"access_token": token, "expires_on": now + 3600}).encode()
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, format: str, *args: object) -> None:
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.endpoint = f"http://127.0.0.1:{self.server.server_port}/token"

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


def docker(arguments: list[str], timeout: int = 120) -> str:
    return subprocess.run(
        ["docker", *arguments],
        capture_output=True,
        check=True,
        timeout=timeout,
        text=True,
    ).stdout.strip()


def mapped_endpoint(container: str, port: int) -> str:
    value = docker(["port", container, f"{port}/tcp"])
    if (
        not re.fullmatch(r"127\.0\.0\.1:[1-9][0-9]{0,4}", value)
        or int(value.rsplit(":", 1)[1]) > 65535
    ):
        raise ValueError("Only one explicit loopback port per emulator service is allowed.")
    return f"https://{value}/devstoreaccount1/"


def native_proof(stdout: bytes) -> dict:
    proofs = []
    for line in stdout.decode().splitlines():
        if "{" in line:
            value = json.loads(line[line.index("{") :])
            if value.get("scope") == SCOPE:
                proofs.append(value)
    if len(proofs) != 1:
        raise ValueError("The exact native contract did not publish one typed proof.")
    proof = proofs[0]
    for field in [
        "table_cas_and_int64",
        "blob_lease_fencing",
        "queue_receipt_rotation_and_redelivery",
        "tls_verified",
        "legacy_queue_worker_and_durable_delivery",
        "expired_cleanup_respects_active_blob_lease",
        "continuous_single_stream_and_exact_samples",
        "audio_retained_until_durable_finalization",
        "continuous_full_replay_after_failure",
    ]:
        if proof.get(field) is not True:
            raise ValueError(f"Native contract did not prove {field}.")
    for field in [
        "production_authorization_proven",
        "real_azure_resources",
        "real_managed_identity",
    ]:
        if proof.get(field) is not False:
            raise ValueError("An emulator fixture must not claim real production authorization.")
    for field in [
        "legacy_recording_id",
        "legacy_transcript_id",
        "continuous_recording_id",
        "continuous_transcript_id",
    ]:
        if str(uuid.UUID(proof[field])) != proof[field]:
            raise ValueError("Native pipeline did not publish canonical synthetic IDs.")
    return proof


def run(
    oracle: Path,
    output: Path,
    revision: str,
    api_oracle: Path | None = None,
    startup_trials: int = 30,
    flows_per_series: int = 100,
) -> None:
    if not sys.platform.startswith("linux") or not oracle.name.startswith("voiceprompt_cloud-"):
        raise ValueError(
            "An explicit Linux cloud libtest fixture is required; normal binaries are forbidden."
        )
    if api_oracle is not None and (
        api_oracle.name != "cloud_contract"
        or not 1 <= startup_trials <= 100
        or not 1 <= flows_per_series <= 1000
    ):
        raise ValueError(
            "An explicit bounded API example, never the production binary, is required."
        )
    output = output.resolve(strict=True)
    if any(output.iterdir()):
        raise FileExistsError("Use a new empty emulator evidence directory.")
    with tempfile.TemporaryDirectory(prefix="VoicePrompt-Azurite-", dir=output) as directory:
        root = Path(directory)
        identity = Identity(certificate(root))
        container = None
        clients = []
        result = None
        api_result = None
        try:
            container = docker(
                [
                    "run",
                    "--detach",
                    "--name",
                    "voiceprompt-fixture-" + uuid.uuid4().hex,
                    "--user",
                    f"{os.getuid()}:{os.getgid()}",
                    "-p",
                    "127.0.0.1::10000",
                    "-p",
                    "127.0.0.1::10001",
                    "-p",
                    "127.0.0.1::10002",
                    "--volume",
                    f"{root}:/fixtures:ro",
                    IMAGE,
                    "azurite",
                    "--blobHost",
                    "0.0.0.0",
                    "--queueHost",
                    "0.0.0.0",
                    "--tableHost",
                    "0.0.0.0",
                    "--location",
                    "/tmp/voiceprompt-data",
                    "--oauth",
                    "basic",
                    "--cert",
                    "/fixtures/server-cert.pem",
                    "--key",
                    "/fixtures/server-key.pem",
                ]
            )
            if not re.fullmatch(r"[a-f0-9]{64}", container):
                raise ValueError("Docker did not return one owned ephemeral container ID.")
            endpoints = {
                name: mapped_endpoint(container, port)
                for name, port in [
                    ("blob", 10000),
                    ("queue", 10001),
                    ("table", 10002),
                ]
            }
            account = dict(
                item.split("=", 1) for item in AZURITE_DEV_CONNECTION_STRING.split(";") if item
            )
            connection = ";".join(
                [
                    "DefaultEndpointsProtocol=https",
                    "AccountName=devstoreaccount1",
                    "AccountKey=" + account["AccountKey"],
                    *[
                        f"{name.title()}Endpoint={value.rstrip('/')}"
                        for name, value in endpoints.items()
                    ],
                ]
            )
            transport = RequestsTransport(
                connection_verify=str(root.joinpath("server-cert.pem")),
                use_env_settings=False,
                connection_timeout=1,
                read_timeout=3,
            )
            blob = BlobServiceClient.from_connection_string(
                connection,
                transport=transport,
                api_version="2023-11-03",
                retry_total=0,
            )
            queue = QueueServiceClient.from_connection_string(
                connection,
                transport=transport,
                api_version="2023-11-03",
                retry_total=0,
            )
            table = TableServiceClient.from_connection_string(
                connection, transport=transport, retry_total=0
            )
            clients = [blob, queue, table]
            until = time.monotonic() + 30
            while True:
                try:
                    blob.get_service_properties()
                    break
                except ServiceRequestError:
                    if time.monotonic() >= until:
                        raise
                    time.sleep(0.25)
            for name in ["audio", "transcripts", "raw"]:
                with suppress(ResourceExistsError):
                    blob.create_container(name)
            for name in ["work", "interop"]:
                with suppress(ResourceExistsError):
                    queue.create_queue(name)
            for name in ["recordings", "chunks", "transcripts"]:
                table.create_table_if_not_exists(name)
            table.get_table_client("recordings").create_entity(
                {
                    "PartitionKey": "synthetic",
                    "RowKey": "python-seed",
                    "counter": EntityProperty(2**40, EdmType.INT64),
                    "small": 7,
                }
            )
            queue.get_queue_client("work").send_message(
                base64.b64encode(
                    json.dumps(
                        {
                            "direction": "python-to-rust",
                            "text": "synthetic \u0159",
                        }
                    ).encode()
                ).decode()
            )
            root.joinpath("fixture.json").write_text(json.dumps({"scope": SCOPE, **endpoints}))
            environment = {
                key: value
                for key, value in os.environ.items()
                if not key.startswith(("AZURE_", "IDENTITY_", "VR_", "VOICEPROMPT_"))
            }
            environment.update(
                {
                    "VOICEPROMPT_AZURITE_CONTRACT_ROOT": str(root),
                    "IDENTITY_ENDPOINT": identity.endpoint,
                    "IDENTITY_HEADER": identity.header,
                    "AZURE_CLIENT_ID": "synthetic-uami",
                }
            )
            native = subprocess.run(
                [str(oracle), TEST, "--exact", "--ignored", "--nocapture", "--test-threads=1"],
                capture_output=True,
                env=environment,
                timeout=120,
                check=False,
            )
            create(output.joinpath("native-stdout.txt"), native.stdout)
            create(output.joinpath("native-stderr.txt"), native.stderr)
            if (
                native.returncode
                or b"1 passed" not in native.stdout
                or b"0 ignored" not in native.stdout
            ):
                raise RuntimeError(
                    "Actual Rust emulator contract failed: "
                    + native.stderr.decode(errors="replace")
                )
            proof = native_proof(native.stdout)
            entity = table.get_table_client("recordings").get_entity("synthetic", "native'quote")
            assert entity["small"] == 8
            assert isinstance(entity["counter"], EntityProperty)
            assert entity["counter"].edm_type == EdmType.INT64
            assert entity["counter"].value == 2**40
            assert entity["text"] == "synthetic \u0159 text"
            recording = table.get_table_client("recordings").get_entity(
                "recording",
                proof["legacy_recording_id"],
            )
            assert (
                recording["state"] == "completed" and recording["stream_delivery_completed"] is True
            )
            transcript = table.get_table_client("transcripts").get_entity(
                "transcript",
                proof["legacy_transcript_id"],
            )
            assert transcript["recording_id"] == proof["legacy_recording_id"]
            assert transcript["source"] == "recording"
            assert isinstance(transcript["completed_at"], datetime)
            assert isinstance(transcript["expires_at"], datetime)
            assert (
                transcript["expires_at"] - transcript["completed_at"]
            ).total_seconds() == 48 * 3600
            body = (
                blob.get_blob_client("transcripts", transcript["body_path"])
                .download_blob()
                .readall()
            )
            assert body == b"synthetic Azure worker final"
            continuous = table.get_table_client("recordings").get_entity(
                "recording",
                proof["continuous_recording_id"],
            )
            assert (
                continuous["state"] == "completed"
                and continuous["stream_delivery_completed"] is True
            )
            assert (
                continuous["transcription_mode"] == "streaming"
                and continuous["audio_layout"] == "contiguous"
            )
            samples = continuous["streamed_samples"]
            assert int(getattr(samples, "value", samples)) == 160320
            assert continuous["stream_attempt"] == 2
            assert continuous.get("stream_lease_id") is None
            continuous_text = table.get_table_client("transcripts").get_entity(
                "transcript",
                proof["continuous_transcript_id"],
            )
            assert continuous_text["recording_id"] == proof["continuous_recording_id"]
            continuous_body = (
                blob.get_blob_client(
                    "transcripts",
                    continuous_text["body_path"],
                )
                .download_blob()
                .readall()
            )
            assert continuous_body == b"synthetic Azure continuous final"
            assert (
                continuous_text["expires_at"] - continuous_text["completed_at"]
            ).total_seconds() == 48 * 3600
            message = next(queue.get_queue_client("interop").receive_messages(messages_per_page=1))
            assert json.loads(base64.b64decode(message.content)) == {
                "direction": "rust-to-python",
                "text": "synthetic \u0159",
            }
            queue.get_queue_client("interop").delete_message(message.id, message.pop_receipt)
            assert identity.resources == ["https://storage.azure.com/"]
            result = {
                "status": "complete",
                "scope": SCOPE,
                "source_revision": revision,
                "emulator_image": IMAGE,
                "native_sha256": hashlib.sha256(oracle.read_bytes()).hexdigest(),
                "tls_certificate_sha256": hashlib.sha256(
                    root.joinpath("server-cert.pem").read_bytes()
                ).hexdigest(),
                "tls_verification_enabled": True,
                "api_version_check_enabled": True,
                "loose_mode": False,
                "actual_azure_sdk_interop": True,
                "table_cas_and_int64": True,
                "blob_lease_fencing": True,
                "queue_receipt_rotation_redelivery": True,
                "legacy_worker_and_notification_settlement": True,
                "cleanup_respects_real_active_blob_lease": True,
                "actual_sdk_worker_transcript_retrieval": True,
                "continuous_worker_single_stream_exact_samples": True,
                "continuous_audio_retained_until_finalization": True,
                "continuous_full_replay_after_failure": True,
                "model_and_realtime_are_loopback_fixtures": True,
                "synthetic_identity_requests": len(identity.resources),
                "real_azure_resources": False,
                "real_managed_identity": False,
                "emulator_validates_token_signatures_or_permissions": False,
                "whole_migration_acceptance": False,
            }
            if api_oracle is not None:
                from azure_api_fixture import AzureProfile
                from measure_backend import Authority, controls, exercise

                profile = AzureProfile(root, endpoints, identity, blob, queue, table)
                authority = Authority()
                try:
                    fixture = profile.fixture("rust", api_oracle, authority)
                    try:
                        controls(fixture, authority)
                        exercise(fixture, authority, 0)
                    finally:
                        fixture.close()
                finally:
                    authority.close()
                observations = profile.observations
                if (
                    len(observations) != 1
                    or observations[0]["implementation"] != "rust"
                    or observations[0]["recordings"] != 1
                    or observations[0]["chunks"] != 1
                    or observations[0]["transcripts"] != 1
                    or observations[0]["queued_messages"] < 1
                    or not observations[0]["resources_deleted"]
                    or observations[0]["storage_backend"] != "azure"
                ):
                    raise ValueError("The independent SDK did not verify the Rust API workload.")
                api_result = {
                    "status": "complete",
                    "scope": "rust-authenticated-https-azurite-api-contract",
                    "source_revision": revision,
                    "emulator_image": IMAGE,
                    "tls_verification_enabled": True,
                    "api_version_check_enabled": True,
                    "resource_observations": observations,
                    "synthetic_identity_requests": len(identity.resources),
                    "performance_profile": False,
                    "real_azure_resources_or_iam": False,
                    "whole_migration_acceptance": False,
                }
        except BaseException as error:
            create(
                output.joinpath("failure.json"),
                json.dumps(
                    {
                        "status": "failed",
                        "error": str(error),
                        "scope": SCOPE,
                        "whole_migration_acceptance": False,
                    },
                    indent=2,
                ).encode(),
            )
            raise
        finally:
            errors = []
            for client in clients:
                try:
                    client.close()
                except Exception as error:
                    errors.append(error)
            try:
                identity.close()
            except Exception as error:
                errors.append(error)
            if container and re.fullmatch(r"[a-f0-9]{64}", container):
                try:
                    create(
                        output.joinpath("emulator-log.txt"), docker(["logs", container]).encode()
                    )
                except Exception as error:
                    errors.append(error)
                try:
                    docker(["rm", "--force", container])
                except Exception as error:
                    errors.append(error)
            if errors:
                create(
                    output.joinpath("cleanup-failure.json"),
                    json.dumps(
                        {
                            "status": "failed",
                            "errors": [str(error) for error in errors],
                            "whole_migration_acceptance": False,
                        },
                        indent=2,
                    ).encode(),
                )
                raise ExceptionGroup("Emulator cleanup failed.", errors)
    if result is None:
        raise RuntimeError("The complete adapter result was not collected.")
    result["owned_emulator_and_identity_stopped"] = True
    result["temporary_private_key_removed"] = True
    create(output.joinpath("azure-rest-contract.json"), json.dumps(result, indent=2).encode())
    if api_result is not None:
        api_result["owned_emulator_and_identity_stopped"] = True
        api_result["temporary_private_key_removed"] = True
        create(
            output.joinpath("rust-azure-api-contract.json"),
            json.dumps(api_result, indent=2).encode(),
        )
    print("Actual HTTPS Azurite, native REST and Azure SDK interop contract passed.", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--oracle", type=Path, required=True)
    parser.add_argument("--output-directory", type=Path, required=True)
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--api-oracle", type=Path)
    parser.add_argument("--startup-trials", type=int, default=30)
    parser.add_argument("--flows-per-series", type=int, default=100)
    args = parser.parse_args()
    run(
        args.oracle.resolve(strict=True),
        args.output_directory,
        args.source_revision,
        args.api_oracle.resolve(strict=True) if args.api_oracle else None,
        args.startup_trials,
        args.flows_per_series,
    )

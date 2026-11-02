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


def run(oracle: Path, output: Path, revision: str) -> None:
    if not sys.platform.startswith("linux") or not oracle.name.startswith("voiceprompt_cloud-"):
        raise ValueError(
            "An explicit Linux cloud libtest fixture is required; normal binaries are forbidden."
        )
    output = output.resolve(strict=True)
    if any(output.iterdir()):
        raise FileExistsError("Use a new empty emulator evidence directory.")
    sys.path.insert(0, str(ROOT.joinpath("backend", "src")))
    from voice_recorder.config import AZURITE_DEV_CONNECTION_STRING

    with tempfile.TemporaryDirectory(prefix="VoicePrompt-Azurite-", dir=output) as directory:
        root = Path(directory)
        identity = Identity(certificate(root))
        container = None
        clients = []
        result = None
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
            entity = table.get_table_client("recordings").get_entity("synthetic", "native'quote")
            assert entity["small"] == 8
            assert isinstance(entity["counter"], EntityProperty)
            assert entity["counter"].edm_type == EdmType.INT64
            assert entity["counter"].value == 2**40
            assert entity["text"] == "synthetic \u0159 text"
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
                "synthetic_identity_requests": len(identity.resources),
                "real_azure_resources": False,
                "real_managed_identity": False,
                "emulator_validates_token_signatures_or_permissions": False,
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
    print("Actual HTTPS Azurite, native REST and Azure SDK interop contract passed.", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--oracle", type=Path, required=True)
    parser.add_argument("--output-directory", type=Path, required=True)
    parser.add_argument("--source-revision", required=True)
    args = parser.parse_args()
    run(args.oracle.resolve(strict=True), args.output_directory, args.source_revision)

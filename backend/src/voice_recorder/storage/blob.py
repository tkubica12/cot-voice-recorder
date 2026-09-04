"""Azure Blob Storage adapter (managed identity or gated Azurite)."""

from __future__ import annotations

import contextlib
from datetime import datetime

from azure.core.exceptions import ResourceExistsError, ResourceNotFoundError
from azure.storage.blob import BlobServiceClient, ContentSettings

from ..errors import BlobNotFound
from ._azure_common import is_transient, wrap_transient


class AzureBlobStore:
    def __init__(self, service: BlobServiceClient, containers: list[str]) -> None:
        self._service = service
        for container in containers:
            with contextlib.suppress(ResourceExistsError):
                self._service.create_container(container)

    def put(self, container: str, path: str, data: bytes, *, content_type: str) -> None:
        client = self._service.get_blob_client(container, path)
        try:
            client.upload_blob(
                data,
                overwrite=True,
                content_settings=ContentSettings(content_type=content_type),
            )
        except Exception as exc:
            raise wrap_transient(exc, "blob upload failed") from exc

    def get(self, container: str, path: str) -> bytes:
        client = self._service.get_blob_client(container, path)
        try:
            downloader = client.download_blob()
            return downloader.readall()
        except ResourceNotFoundError as exc:
            raise BlobNotFound(f"{container}/{path}") from exc
        except Exception as exc:
            raise wrap_transient(exc, "blob download failed") from exc

    def delete(self, container: str, path: str) -> bool:
        client = self._service.get_blob_client(container, path)
        try:
            client.delete_blob()
            return True
        except ResourceNotFoundError:
            return False
        except Exception as exc:
            raise wrap_transient(exc, "blob delete failed") from exc

    def exists(self, container: str, path: str) -> bool:
        client = self._service.get_blob_client(container, path)
        try:
            return bool(client.exists())
        except Exception as exc:
            raise wrap_transient(exc, "blob exists failed") from exc

    def list_paths(self, container: str, *, prefix: str = "") -> list[tuple[str, datetime]]:
        client = self._service.get_container_client(container)
        try:
            return [
                (blob.name, blob.last_modified)
                for blob in client.list_blobs(name_starts_with=prefix or None)
            ]
        except Exception as exc:
            if is_transient(exc):
                raise wrap_transient(exc, "blob list failed") from exc
            raise

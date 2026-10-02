from __future__ import annotations

import asyncio
import json
import uuid
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from datetime import timedelta

import pytest
from fastapi.testclient import TestClient

from voice_recorder.ai.fakes import FakeDictationRefiner, FakeTranscriber
from voice_recorder.ai.streaming import UpstreamStream
from voice_recorder.app import create_app
from voice_recorder.models import DictationArchiveRequest
from voice_recorder.services.cleanup import run_cleanup
from voice_recorder.services.dictation_archive import archive

PREFIX = "conversation.item.input_audio_transcription."


class FakeStream:
    def __init__(self) -> None:
        self.events: asyncio.Queue[str] = asyncio.Queue()
        self.audio = []
        self.commits = 0

    async def send(self, message: str) -> None:
        data = json.loads(message)
        if data["type"] == "input_audio_buffer.append":
            self.audio.append(data["audio"])
            await self.events.put(
                json.dumps(
                    {
                        "type": PREFIX + "intermediate",
                        "item_id": f"item-{self.commits}",
                        "intermediate": "old",
                    }
                )
            )
            await self.events.put(
                json.dumps(
                    {
                        "type": PREFIX + "intermediate",
                        "item_id": f"item-{self.commits}",
                        "intermediate": "new",
                    }
                )
            )
        elif data["type"] == "input_audio_buffer.commit":
            identity = f"item-{self.commits}"
            self.commits += 1
            await self.events.put(
                json.dumps({"type": "input_audio_buffer.committed", "item_id": identity})
            )
            await self.events.put(
                json.dumps(
                    {
                        "type": PREFIX + "completed",
                        "item_id": identity,
                        "transcript": "Final text.",
                    }
                )
            )

    async def recv(self) -> str:
        return await self.events.get()


class FakeProvider:
    def __init__(self) -> None:
        self.stream = FakeStream()
        self.languages: list[str] = []

    @asynccontextmanager
    async def session(self, language: str) -> AsyncIterator[UpstreamStream]:
        self.languages.append(language)
        yield self.stream

    async def close(self) -> None:
        pass


def streaming_client(context, verifier, provider: FakeProvider) -> TestClient:
    return TestClient(
        create_app(
            context=context,
            verifier=verifier,
            dictation_transcriber=FakeTranscriber(),
            dictation_refiner=FakeDictationRefiner(),
            streaming_provider=provider,
        )
    )


def test_relay_configures_server_side_and_tracks_absolute_source_cursor(context, verifier, auth):
    provider = FakeProvider()
    with (
        streaming_client(context, verifier, provider) as client,
        client.websocket_connect(
            "/v1/dictation/stream?language=cs&start_byte=32000", headers=auth
        ) as ws,
    ):
        assert ws.receive_json() == {"type": "ready", "start_byte": 32000, "protocol": 1}
        ws.send_bytes(bytes(1280))
        assert ws.receive_json()["text"] == "old"
        assert ws.receive_json()["text"] == "new"
        ws.send_json({"type": "finish"})
        confirmed = ws.receive_json()
        assert confirmed == {
            "type": "confirmed",
            "item_id": "item-0",
            "byte_end": 33280,
            "text": "Final text.",
        }
        assert ws.receive_json() == {"type": "done", "byte_end": 33280}
    assert provider.languages == ["cs"]
    assert len(provider.stream.audio) == 1
    assert not context.transcripts.list_page(limit=10, cursor=None)[0]
    assert not context.queue.receive(max_messages=1, visibility_seconds=30)


@pytest.mark.parametrize("headers", [{}, {"Authorization": "Bearer forbidden-token"}])
def test_websocket_auth_rejects_before_upstream_access(context, verifier, headers):
    provider = FakeProvider()
    with (
        streaming_client(context, verifier, provider) as client,
        client.websocket_connect("/v1/dictation/stream", headers=headers) as ws,
    ):
        assert ws.receive_json()["code"] in {"unauthorized", "forbidden"}
    assert not provider.languages


@pytest.mark.parametrize("packet", [b"", b"\x00", bytes(16386)], ids=["empty", "odd", "oversized"])
def test_invalid_pcm_is_explicit_and_never_sent_upstream(context, verifier, auth, packet):
    provider = FakeProvider()
    with (
        streaming_client(context, verifier, provider) as client,
        client.websocket_connect("/v1/dictation/stream", headers=auth) as ws,
    ):
        ws.receive_json()
        ws.send_bytes(packet)
        assert ws.receive_json()["type"] == "error"
    assert not provider.stream.audio


def audit_body(clock):
    return {
        "text": "Clean text.",
        "raw_text": "Original text.",
        "language": "cs",
        "completed_at": clock.now().isoformat(),
        "polished": False,
    }


def test_audit_is_idempotent_retrievable_and_does_not_notify(
    client, auth, context, clock, realtime
):
    session = uuid.uuid4()
    body = audit_body(clock)
    response = client.put(f"/v1/dictation/transcripts/{session.hex}", json=body, headers=auth)
    assert response.status_code == 200
    assert response.headers["cache-control"] == "no-store"
    assert response.json()["source"] == "windows_dictation"
    assert response.json()["refine_model"] == "none"
    assert response.json()["transcribe_model"] == "MAI-Transcribe-2-Streaming"
    expires = response.json()["expires_at"]
    clock.advance(hours=1)
    replay = client.put(f"/v1/dictation/transcripts/{session}", json=body, headers=auth)
    assert replay.status_code == 200 and replay.json()["expires_at"] == expires
    fetched = client.get(f"/v1/transcripts/{session}", headers=auth)
    assert fetched.json()["body"] == "Clean text."
    assert fetched.json()["raw_body"] == "Original text."
    assert fetched.json()["source"] == "windows_dictation"
    assert (
        client.get("/v1/transcripts", headers=auth).json()["items"][0]["source"]
        == "windows_dictation"
    )
    assert len(context.transcripts.list_page(limit=10, cursor=None)[0]) == 1
    assert not realtime.sent


def test_audit_conflict_cannot_overwrite_original(client, auth, clock):
    session = uuid.uuid4()
    body = audit_body(clock)
    assert (
        client.put(f"/v1/dictation/transcripts/{session}", json=body, headers=auth).status_code
        == 200
    )
    changed = body | {"text": "Different text."}
    assert (
        client.put(f"/v1/dictation/transcripts/{session}", json=changed, headers=auth).status_code
        == 409
    )
    assert client.get(f"/v1/transcripts/{session}", headers=auth).json()["body"] == body["text"]


def test_audit_expires_and_body_is_removed(client, auth, context, clock):
    session = uuid.uuid4()
    response = client.put(
        f"/v1/dictation/transcripts/{session}", json=audit_body(clock), headers=auth
    )
    assert response.status_code == 200
    meta = context.transcripts.get(str(session))
    assert meta is not None
    clock.advance(hours=48)
    assert client.get(f"/v1/transcripts/{session}", headers=auth).status_code == 404
    assert run_cleanup(context).transcripts_deleted == 1
    assert context.transcripts.get(str(session)) is None
    assert not context.blobs.exists(context.settings.transcript_container, meta.body_path)


def test_audit_rejects_duplicate_keys_naive_dates_and_future_completion(client, auth, clock):
    path = f"/v1/dictation/transcripts/{uuid.uuid4()}"
    body = audit_body(clock)
    for bad in [
        body | {"completed_at": clock.now().replace(tzinfo=None).isoformat()},
        body | {"completed_at": (clock.now() + timedelta(hours=1)).isoformat()},
        body | {"raw_text": ""},
    ]:
        assert client.put(path, json=bad, headers=auth).status_code == 422
    duplicated = json.dumps(body)[:-1] + ',"text":"replacement"}'
    assert (
        client.put(
            path, content=duplicated, headers=auth | {"Content-Type": "application/json"}
        ).status_code
        == 422
    )


def test_audit_blob_failure_cannot_publish_a_successful_metadata_row(context, clock, monkeypatch):
    def fail_put(*args, **kwargs):
        raise OSError("Injected storage failure")

    monkeypatch.setattr(context.blobs, "put", fail_put)
    identifier = str(uuid.uuid4())
    with pytest.raises(OSError):
        archive(context, identifier, DictationArchiveRequest.model_validate(audit_body(clock)))
    assert context.transcripts.get(identifier) is None


def test_orphan_audit_expires_from_completion_not_a_late_upload_timestamp(context, clock):
    path = f"windows-dictation/{uuid.uuid4()}/orphan.json"
    context.blobs.put(
        context.settings.transcript_container,
        path,
        json.dumps({"completed_at": clock.now().isoformat()}).encode(),
        content_type="application/json",
    )
    clock.advance(hours=49)
    run_cleanup(context)
    assert not context.blobs.exists(context.settings.transcript_container, path)

"""Bounded authenticated PCM relay. Audit is a separate durable final-text PUT."""

from __future__ import annotations

import asyncio
import base64
import json
from collections import deque
from typing import Any

from fastapi import APIRouter, WebSocket, WebSocketDisconnect
from starlette.websockets import WebSocketState

from ..ai.streaming import UpstreamStream
from ..auth import TokenVerifier
from ..dictation_edits import strict_json_object
from ..logging_config import get_logger
from ..problems import ProblemError

router = APIRouter(prefix="/v1/dictation", tags=["dictation"])
logger = get_logger(__name__)
PREFIX = "conversation.item.input_audio_transcription."
MAX_PACKET_BYTES = 16384
MAX_TEXT_CHARS = 1_048_576


async def relay(client: WebSocket, upstream: UpstreamStream, start_byte: int) -> None:
    cursor = start_byte
    committed_through = start_byte
    pending: deque[int] = deque()
    order: deque[int] = deque()
    by_item: dict[str, int] = {}
    completed: dict[int, tuple[str, str]] = {}
    finish = asyncio.Event()
    all_done = asyncio.Event()
    all_done.set()

    async def commit() -> None:
        nonlocal committed_through
        if cursor == committed_through:
            return
        if len(order) >= 32:
            raise ValueError("Too many pending transcription commits")
        pending.append(cursor)
        order.append(cursor)
        all_done.clear()
        committed_through = cursor
        await upstream.send(json.dumps({"type": "input_audio_buffer.commit"}))

    async def send_audio() -> None:
        nonlocal cursor
        while not finish.is_set():
            message = await client.receive()
            if message["type"] == "websocket.disconnect":
                raise WebSocketDisconnect(message.get("code", 1006))
            audio = message.get("bytes")
            if audio is not None:
                if not audio or len(audio) > MAX_PACKET_BYTES or len(audio) % 2:
                    raise ValueError("Invalid PCM packet")
                cursor += len(audio)
                if cursor - start_byte > 32000 * 3600:
                    raise ValueError("Streaming session audio limit exceeded")
                await upstream.send(
                    json.dumps(
                        {
                            "type": "input_audio_buffer.append",
                            "audio": base64.b64encode(audio).decode("ascii"),
                        }
                    )
                )
                continue
            text = message.get("text")
            if text is None or len(text) > 256:
                raise ValueError("Invalid streaming control")
            data = json.loads(text, object_pairs_hook=strict_json_object)
            if data not in ({"type": "commit"}, {"type": "finish"}):
                raise ValueError("Unsupported streaming control")
            await commit()
            if data["type"] == "finish":
                finish.set()
                return

    async def receive_text() -> None:
        while True:
            event: dict[str, Any] = json.loads(await upstream.recv())
            kind = event.get("type")
            if kind in {"error", PREFIX + "failed"}:
                raise RuntimeError("Upstream transcription failed")
            item = event.get("item_id")
            if kind not in {
                PREFIX + "delta",
                PREFIX + "intermediate",
                PREFIX + "completed",
                "input_audio_buffer.committed",
            }:
                continue
            if not isinstance(item, str):
                raise RuntimeError("Missing upstream item identity")
            if kind in {PREFIX + "delta", PREFIX + "intermediate"}:
                field = "delta" if kind.endswith(".delta") else "intermediate"
                value = event.get(field)
                if not isinstance(value, str) or len(value) > MAX_TEXT_CHARS:
                    raise RuntimeError("Invalid upstream hypothesis")
                await client.send_json({"type": field, "item_id": item, "text": value})
            elif kind == "input_audio_buffer.committed":
                if not pending or item in by_item:
                    raise RuntimeError("Unexpected upstream commit acknowledgement")
                by_item[item] = pending.popleft()
            elif kind == PREFIX + "completed":
                value = event.get("transcript")
                if item not in by_item or not isinstance(value, str) or len(value) > MAX_TEXT_CHARS:
                    raise RuntimeError("Invalid upstream completion")
                end = by_item.pop(item)
                completed[end] = (item, value)
                while order and order[0] in completed:
                    through = order.popleft()
                    identity, transcript = completed.pop(through)
                    await client.send_json(
                        {
                            "type": "confirmed",
                            "item_id": identity,
                            "byte_end": through,
                            "text": transcript,
                        }
                    )
                if not order:
                    all_done.set()

    async with asyncio.timeout(55 * 60):
        sender = asyncio.create_task(send_audio())
        receiver = asyncio.create_task(receive_text())
        try:
            done, _ = await asyncio.wait((sender, receiver), return_when=asyncio.FIRST_COMPLETED)
            for task in done:
                await task
            if not finish.is_set():
                raise RuntimeError("Streaming receiver ended prematurely")

            async def wait_until_done() -> None:
                await all_done.wait()

            drain = asyncio.create_task(wait_until_done())
            try:
                done, _ = await asyncio.wait(
                    (drain, receiver), timeout=30, return_when=asyncio.FIRST_COMPLETED
                )
                for task in done:
                    await task
                if drain not in done:
                    raise TimeoutError("Streaming completion deadline exceeded")
                await client.send_json({"type": "done", "byte_end": cursor})
            finally:
                drain.cancel()
                await asyncio.gather(drain, return_exceptions=True)
        finally:
            sender.cancel()
            receiver.cancel()
            await asyncio.gather(sender, receiver, return_exceptions=True)


@router.websocket("/stream")
async def stream_dictation(websocket: WebSocket) -> None:
    async def reject(code: str, close_code: int) -> None:
        await websocket.accept()
        await websocket.send_json({"type": "error", "code": code})
        await websocket.close(code=close_code)

    authorization = websocket.headers.get("authorization", "")
    scheme, _, token = authorization.partition(" ")
    try:
        if scheme.lower() != "bearer" or not token.strip():
            await reject("unauthorized", 4401)
            return
        verifier: TokenVerifier = websocket.app.state.verifier
        await asyncio.to_thread(verifier.verify, token.strip())
    except ProblemError as exc:
        await reject(
            "forbidden" if exc.status == 403 else "unauthorized",
            4403 if exc.status == 403 else 4401,
        )
        return
    language = websocket.query_params.get("language", "auto")
    try:
        start = int(websocket.query_params.get("start_byte", "0"))
    except ValueError:
        start = -1
    if language not in {"auto", "cs", "en"} or start < 0 or start % 2 or start > 48 * 3600 * 32000:
        await reject("invalid_request", 4400)
        return
    provider = websocket.app.state.dictation_streaming
    slots: asyncio.Semaphore = websocket.app.state.dictation_stream_slots
    if provider is None or slots.locked():
        await reject(
            "too_many_requests" if provider is not None else "unavailable",
            4429 if provider is not None else 4503,
        )
        return
    await slots.acquire()
    try:
        await websocket.accept()
        async with provider.session(language) as upstream:
            await websocket.send_json({"type": "ready", "start_byte": start, "protocol": 1})
            await relay(websocket, upstream, start)
    except WebSocketDisconnect:
        logger.info("dictation_stream_disconnected")
    except Exception as exc:
        # Neither provider exception text nor audio/text/credentials belongs in logs.
        logger.warning("dictation_stream_failed", extra={"error_type": type(exc).__name__})
        if websocket.application_state == WebSocketState.CONNECTED:
            try:
                await websocket.send_json(
                    {
                        "type": "error",
                        "code": "stream_failed",
                        "message": "Streaming interrupted. Saved audio can be replayed.",
                    }
                )
                await websocket.close(code=1011)
            except (RuntimeError, WebSocketDisconnect, OSError):
                logger.info("dictation_stream_error_delivery_failed")
    finally:
        slots.release()

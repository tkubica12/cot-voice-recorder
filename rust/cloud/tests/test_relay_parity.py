"""Actual asynchronous Python relay -> Rust cursor and confirmation semantics."""

from __future__ import annotations

import argparse
import asyncio
import base64
import json
import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3].joinpath("backend", "src")))
from voice_recorder.routers.dictation_stream import PREFIX, relay

ORACLE: Path


class Client:
    def __init__(self, frames: list[dict[str, object]]) -> None:
        self.frames = iter(frames)
        self.messages: list[dict[str, object]] = []

    async def receive(self) -> dict[str, object]:
        return next(self.frames)

    async def send_json(self, data: dict[str, object]) -> None:
        self.messages.append(data)


class Upstream:
    def __init__(self, duplicate_ack: bool) -> None:
        self.events: asyncio.Queue[str] = asyncio.Queue()
        self.commits = 0
        self.audio = bytearray()
        self.duplicate_ack = duplicate_ack

    async def send(self, message: str) -> None:
        event = json.loads(message)
        if event["type"] == "input_audio_buffer.append":
            self.audio.extend(base64.b64decode(event["audio"]))
        elif event["type"] == "input_audio_buffer.commit":
            self.commits += 1
            item = "first" if self.commits == 1 or self.duplicate_ack else "second"
            await self.events.put(
                json.dumps({"type": "input_audio_buffer.committed", "item_id": item})
            )
            if self.commits == 1:
                await self.events.put(
                    json.dumps(
                        {
                            "type": PREFIX + "delta",
                            "item_id": "first",
                            "delta": "provisional",
                        }
                    )
                )
            if self.commits == 2 and not self.duplicate_ack:
                for item in ["second", "first"]:
                    await self.events.put(
                        json.dumps(
                            {
                                "type": PREFIX + "completed",
                                "item_id": item,
                                "transcript": item + " text",
                            }
                        )
                    )

    async def recv(self) -> str:
        return await self.events.get()


class RelayParityTests(unittest.IsolatedAsyncioTestCase):
    async def test_actual_python_relay_and_rust_capture_order_not_ack_order(
        self,
    ) -> None:
        for duplicate in [False, True]:
            with self.subTest(duplicate=duplicate):
                first = bytes([1, 2, 3, 4])
                second = bytes([5, 6])
                client = Client(
                    [
                        {"type": "websocket.receive", "bytes": first},
                        {"type": "websocket.receive", "text": '{"type":"commit"}'},
                        {"type": "websocket.receive", "bytes": second},
                        {"type": "websocket.receive", "text": '{"type":"finish"}'},
                    ]
                )
                upstream = Upstream(duplicate)
                accepted = True
                try:
                    await asyncio.wait_for(relay(client, upstream, 1000), timeout=5)
                except (ValueError, RuntimeError):
                    accepted = False
                actions = [
                    {"kind": "audio", "data": base64.b64encode(first).decode()},
                    {"kind": "control", "data": '{"type":"commit"}'},
                    {"kind": "audio", "data": base64.b64encode(second).decode()},
                    {"kind": "control", "data": '{"type":"finish"}'},
                    {
                        "kind": "event",
                        "data": {
                            "type": "input_audio_buffer.committed",
                            "item_id": "first",
                        },
                    },
                    {
                        "kind": "event",
                        "data": {
                            "type": PREFIX + "delta",
                            "item_id": "first",
                            "delta": "provisional",
                        },
                    },
                    {
                        "kind": "event",
                        "data": {
                            "type": "input_audio_buffer.committed",
                            "item_id": "first" if duplicate else "second",
                        },
                    },
                ]
                if not duplicate:
                    for item in ["second", "first"]:
                        actions.append(
                            {
                                "kind": "event",
                                "data": {
                                    "type": PREFIX + "completed",
                                    "item_id": item,
                                    "transcript": item + " text",
                                },
                            }
                        )
                result = await asyncio.to_thread(
                    subprocess.run,
                    [str(ORACLE), "relay"],
                    input=json.dumps({"start": 1000, "actions": actions}).encode(),
                    capture_output=True,
                    timeout=10,
                    check=False,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                actual = json.loads(result.stdout)
                self.assertEqual(actual["accepted"], accepted)
                self.assertEqual(actual["messages"], client.messages)
                self.assertEqual(bytes(upstream.audio), first + second)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--oracle", type=Path, required=True)
    args = parser.parse_args()
    ORACLE = args.oracle.resolve(strict=True)
    unittest.main(argv=[__file__], verbosity=2)

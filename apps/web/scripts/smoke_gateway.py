"""Протокольный смок шлюза (этап H): один rchat-server, два профиля.

Проверяет:
  * обычный HTTP (не /ws) получает JSON 404 от шлюза;
  * `ping` → `ok` с `service == "rchat-ffi"`;
  * `register` для alice и bob проходит, аватар содержит инициалы;
  * alice создаёт прямый E2E-чат с bob и отправляет сообщение;
  * bob получает `chat_upserted` и `message_received` с телом сообщения
    (рассылка событий и порядок ok/error сохранены).

Запуск (нужен собранный сервер и пакет websockets>=12):

    cargo build --bin rchat-server
    python3 -m venv .venv && .venv/bin/pip install websockets
    .venv/bin/python apps/web/scripts/smoke_gateway.py
"""

from __future__ import annotations

import asyncio
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

REPO = Path(__file__).resolve().parents[3]
SERVER_BIN = Path(os.environ.get("RCHAT_SERVER_BIN", str(REPO / "target/debug/rchat-server")))

sys.path.insert(0, str(REPO / "bindings/python"))

from websockets.asyncio.server import serve  # noqa: E402
from websockets.exceptions import ConnectionClosed  # noqa: E402
from websockets.sync.client import connect  # noqa: E402

from rchat.gateway import WebSocketBridge, _reject_plain_http  # noqa: E402


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def wait_tcp(port: int, timeout: float = 40.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.3):
                return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError(f"ничего не слушает порт {port}")


async def _client_loop(bridge: WebSocketBridge, connection: Any) -> None:
    bridge.connections.add(connection)
    try:
        async for raw in connection:
            response = bridge.dispatch_client_json(raw)
            await connection.send(json.dumps(response))
    except ConnectionClosed:
        pass
    finally:
        bridge.connections.discard(connection)


async def gateway_pair(
    alice_port: int,
    bob_port: int,
    data_dirs: list[Path],
    server_url: str,
    ready: asyncio.Event,
    done: asyncio.Event,
) -> None:
    alice = WebSocketBridge(data_dir=str(data_dirs[0]), server=server_url)
    bob = WebSocketBridge(data_dir=str(data_dirs[1]), server=server_url)

    async def alice_handler(connection: Any) -> None:
        await _client_loop(alice, connection)

    async def bob_handler(connection: Any) -> None:
        await _client_loop(bob, connection)

    try:
        async with serve(
            alice_handler, "127.0.0.1", alice_port, process_request=_reject_plain_http
        ), serve(
            bob_handler, "127.0.0.1", bob_port, process_request=_reject_plain_http
        ):
            relay_a = asyncio.create_task(alice.relay_loop())
            relay_b = asyncio.create_task(bob.relay_loop())
            ready.set()
            try:
                await done.wait()
            finally:
                relay_a.cancel()
                relay_b.cancel()
    finally:
        alice.close()
        bob.close()


def recv_until(ws: Any, predicate: Any, timeout: float = 20.0) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    while True:
        remaining = max(0.05, deadline - time.monotonic())
        message = json.loads(ws.recv(timeout=remaining))
        if predicate(message):
            return message
        if time.monotonic() >= deadline:
            raise TimeoutError("условие не выполнено за отведённое время")


def client_body(alice_port: int, bob_port: int) -> None:
    """Блокирующая часть проверок (выполняется в потоке-исполнителе)."""
    with connect(f"ws://127.0.0.1:{alice_port}/ws", open_timeout=5) as alice, connect(
        f"ws://127.0.0.1:{bob_port}/ws", open_timeout=5
    ) as bob:
        # 1) обычный HTTP на том же порту — вежливый JSON 404
        probe = socket.create_connection(("127.0.0.1", alice_port), timeout=5)
        probe.sendall(b"GET /nope HTTP/1.1\r\nHost: x\r\n\r\n")
        head = probe.recv(4096)
        assert b"404" in head.split(b"\r\n", 1)[0], head
        probe.close()
        print("HTTP 404 на /nope — ok")

        # 2) ping
        alice.send('{"@type":"ping"}')
        ping = recv_until(alice, lambda m: m["@type"] == "ok" and m["request"] == "ping")
        assert ping["data"]["service"] == "rchat-ffi", ping
        print("ping — ok")

        # 3) регистрация обоих пользователей
        alice.send(json.dumps({
            "@type": "register",
            "username": "alice",
            "password": "pw-alice-1",
            "display_name": "Alice",
        }))
        session = recv_until(alice, lambda m: m["@type"] == "ok")
        assert session["data"]["username"] == "alice", session
        assert session["data"]["avatar"]["initials"], session

        bob.send(json.dumps({
            "@type": "register",
            "username": "bob",
            "password": "pw-bob-1",
            "display_name": "Bob",
        }))
        recv_until(bob, lambda m: m["@type"] == "ok")
        print("register обеих сторон — ok")

        # 4) прямой чат и сообщение. По умолчанию — облачный режим;
        #    приватный (E2E) включается режимом `private`.
        alice.send(
            json.dumps(
                {
                    "@type": "create_direct_chat",
                    "peer_username": "bob",
                    "mode": "private",
                }
            )
        )
        chat = recv_until(
            alice, lambda m: m["@type"] == "ok" and m["request"] == "create_direct_chat"
        )
        chat_id = chat["data"]["chat_id"]
        assert chat["data"]["e2e_mode"] == "olm_v1", chat
        assert chat["data"]["privacy"] == "private", chat

        text = "привет из smoke_gateway"
        alice.send(
            json.dumps({"@type": "send_message", "chat_id": chat_id, "text": text})
        )
        sent = recv_until(
            alice, lambda m: m["@type"] == "ok" and m["request"] == "send_message"
        )
        assert sent["data"]["local_id"], sent

        # 5) у Боба приходит chat_upserted, затем message_received
        started = time.monotonic()
        recv_until(bob, lambda m: m.get("event") == "chat_upserted")
        received = recv_until(bob, lambda m: m.get("event") == "message_received")
        assert received["data"]["body"] == text, received
        assert received["data"]["chat_id"] == chat_id, received
        print(f"сообщение доставлено Бобу за {time.monotonic() - started:.2f} с")


async def amain() -> None:
    loop = asyncio.get_running_loop()
    with tempfile.TemporaryDirectory(prefix="rchat-gw-smoke-") as tmp:
        root = Path(tmp)
        server_port = free_port()
        server_log_path = root / "server.log"
        server_log = open(server_log_path, "wb")
        server = subprocess.Popen(
            [
                str(SERVER_BIN),
                "--db", str(root / "server.sqlite3"),
                "--listen", f"127.0.0.1:{server_port}",
                "--plaintext-insecure",
                "--allow-dev-plaintext-chats",
            ],
            stdout=server_log,
            stderr=subprocess.STDOUT,
        )
        try:
            try:
                await asyncio.wait_for(
                    _tcp_ready(loop, server_port, server), timeout=40
                )
            except TimeoutError as error:
                details = server_log_path.read_text(encoding="utf-8", errors="replace")
                raise RuntimeError(f"сервер не запустился:\n{details}") from error

            alice_port, bob_port = free_port(), free_port()
            ready, done = asyncio.Event(), asyncio.Event()
            pair = asyncio.create_task(
                gateway_pair(
                    alice_port,
                    bob_port,
                    [root / "alice", root / "bob"],
                    f"ws://127.0.0.1:{server_port}",
                    ready,
                    done,
                )
            )
            try:
                await asyncio.wait_for(ready.wait(), timeout=10)
                await asyncio.gather(
                    _tcp_ready(loop, alice_port),
                    _tcp_ready(loop, bob_port),
                )
                await loop.run_in_executor(None, client_body, alice_port, bob_port)
            finally:
                done.set()
                await asyncio.wait_for(asyncio.shield(pair), timeout=10)
            print("SMOKE GATEWAY OK")
        finally:
            server.terminate()
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill()
            server_log.close()


async def _tcp_ready(
    loop: asyncio.AbstractEventLoop,
    port: int,
    process: subprocess.Popen[bytes] | None = None,
) -> None:
    deadline = loop.time() + 10
    while loop.time() < deadline:
        if process is not None and process.poll() is not None:
            raise RuntimeError("rchat-server завершился раньше времени")
        try:
            reader, writer = await asyncio.open_connection("127.0.0.1", port)
            writer.close()
            return
        except OSError:
            await asyncio.sleep(0.1)
    raise TimeoutError(f"порт {port} не открылся")


if __name__ == "__main__":
    asyncio.run(amain())

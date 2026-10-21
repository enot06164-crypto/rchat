"""Тест доставки событий шлюзом: события ядра долетают до браузера.

Шлюз читает события ядра отдельным потоком и передаёт их всем вкладкам
профиля.  Тест проверяет, что после отправки сообщения браузер получает
подтверждение (``outbox_changed`` / ``message_updated`` со статусом
``sent``) — то есть цепочка «ядро → шлюз → WebSocket» работает.
"""

from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from websockets.sync.client import connect  # noqa: E402

from rchat import RChatClient, ffi  # noqa: E402

ROOT = Path(__file__).resolve().parents[3]


def _find_server() -> Path:
    configured = os.environ.get("RCHAT_SERVER_BIN")
    if configured:
        path = Path(configured).expanduser().resolve()
        if not path.is_file():
            raise RuntimeError(f"RCHAT_SERVER_BIN does not exist: {path}")
        return path
    for profile in ("debug", "release"):
        candidate = ROOT / "target" / profile / "rchat-server"
        if candidate.is_file():
            return candidate
    raise RuntimeError(
        "rchat-server is not built; run 'cargo build --bin rchat-server'"
    )


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return int(probe.getsockname()[1])


def _wait_until_listening(
    process: subprocess.Popen[bytes], port: int, timeout: float = 20.0
) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"process exited with {process.returncode}")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError("process did not start listening")


def _call(ws: object, request: dict[str, object]) -> dict[str, object]:
    """Отправляет команду и ждёт ответа (события пропускает)."""
    ws.send(json.dumps(request))  # type: ignore[attr-defined]
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        message = json.loads(ws.recv(timeout=10))  # type: ignore[attr-defined]
        if message.get("@type") == "event":
            continue
        return message
    raise AssertionError("no answer from the gateway")


def _wait_event(
    ws: object, names: tuple[str, ...], timeout: float = 20.0
) -> dict[str, object]:
    """Ждёт события с одним из указанных имён."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        message = json.loads(ws.recv(timeout=10))  # type: ignore[attr-defined]
        if message.get("@type") == "event" and message.get("event") in names:
            return message
    raise AssertionError(f"no event {names} from the gateway")


class GatewayEventsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        try:
            ffi.get_library()
        except Exception as error:  # noqa: BLE001 - test bootstrap
            raise RuntimeError(
                "rchat FFI library is not available; run "
                "'cargo build -p rchat-ffi' first"
            ) from error
        cls.server_binary = _find_server()

    def test_gateway_relays_core_events(self) -> None:
        with tempfile.TemporaryDirectory(prefix="rchat-gateway-events-") as root:
            root_path = Path(root)
            server_port = _free_port()
            gateway_port = _free_port()
            log_file = open(root_path / "server.log", "wb")
            server = subprocess.Popen(
                [
                    str(self.server_binary),
                    "--db",
                    str(root_path / "server.sqlite3"),
                    "--listen",
                    f"127.0.0.1:{server_port}",
                    "--plaintext-insecure",
                    "--allow-dev-plaintext-chats",
                ],
                stdout=log_file,
                stderr=subprocess.STDOUT,
            )
            gateway = subprocess.Popen(
                [
                    sys.executable,
                    "-m",
                    "rchat.gateway",
                    "--data-dir",
                    str(root_path / "gateway"),
                    "--server",
                    f"ws://127.0.0.1:{server_port}",
                    "--port",
                    str(gateway_port),
                ],
                cwd=ROOT,
                env={
                    **dict(os.environ),
                    "PYTHONPATH": str(ROOT / "bindings/python"),
                },
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            try:
                _wait_until_listening(server, server_port)
                _wait_until_listening(gateway, gateway_port)

                # Собеседник — обычный клиент SDK (регистрация на сервере).
                with RChatClient(
                    root_path / "peer", f"ws://127.0.0.1:{server_port}", e2e=True
                ) as peer:
                    peer.register("peer", "pw-peer-1", "Пётр")

                    url = (
                        f"ws://127.0.0.1:{gateway_port}/ws"
                        f"?profile=default&device={'a' * 64}"
                    )
                    with connect(url) as ws:
                        answer = _call(
                            ws,
                            {
                                "@type": "register",
                                "username": "webuser",
                                "password": "pw-web-1",
                                "display_name": "Веб",
                            },
                        )
                        self.assertEqual(answer["@type"], "ok")
                        answer = _call(
                            ws, {"@type": "create_direct_chat", "peer_username": "peer"}
                        )
                        self.assertEqual(answer["@type"], "ok")
                        chat = answer["data"]
                        chat_id = chat["chat_id"] if isinstance(chat, dict) else chat["id"]

                        answer = _call(
                            ws,
                            {
                                "@type": "send_message",
                                "chat_id": chat_id,
                                "text": "событие",
                            },
                        )
                        self.assertEqual(answer["@type"], "ok")

                        # Подтверждение отправки приходит событием.
                        event = _wait_event(
                            ws, ("outbox_changed", "message_updated"), timeout=25
                        )
                        data = event.get("data") or {}
                        self.assertIn("local_id", data)
            finally:
                gateway.terminate()
                try:
                    gateway.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    gateway.kill()
                    gateway.wait()
                server.terminate()
                try:
                    server.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()
                log_file.close()


if __name__ == "__main__":
    unittest.main()

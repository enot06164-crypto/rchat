"""End-to-end test: two independent Python clients over a real rchat server.

The test starts ``target/debug/rchat-server`` (or a path from
``RCHAT_SERVER_BIN``) in a temporary directory with plaintext transport and
dev-chat support enabled.  The E2E chat itself still uses Olm.
"""

from __future__ import annotations

import os
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from rchat import AvatarInfo, Event, Message, RChatClient, ffi  # noqa: E402

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
    process: subprocess.Popen[bytes],
    port: int,
    log_path: Path,
    timeout: float = 20.0,
) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            details = log_path.read_text(encoding="utf-8", errors="replace")
            raise RuntimeError(
                f"rchat-server exited with {process.returncode}:\n{details}"
            )
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.05)
    details = log_path.read_text(encoding="utf-8", errors="replace")
    raise RuntimeError(f"rchat-server did not listen on port {port}:\n{details}")


class ClientE2ETests(unittest.TestCase):
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

    def test_two_clients_register_and_exchange_e2e_message(self) -> None:
        text = "привет из Python SDK"
        with tempfile.TemporaryDirectory(prefix="rchat-python-e2e-") as root:
            root_path = Path(root)
            log_path = root_path / "server.log"
            port = _free_port()
            command = [
                str(self.server_binary),
                "--db",
                str(root_path / "server.sqlite3"),
                "--listen",
                f"127.0.0.1:{port}",
                "--plaintext-insecure",
                "--allow-dev-plaintext-chats",
            ]
            log_file = open(log_path, "wb")
            process = subprocess.Popen(
                command,
                stdout=log_file,
                stderr=subprocess.STDOUT,
            )
            try:
                _wait_until_listening(process, port, log_path)
                url = f"ws://127.0.0.1:{port}"

                with RChatClient(
                    root_path / "alice", url, e2e=True
                ) as alice, RChatClient(
                    root_path / "bob", url, e2e=True
                ) as bob:
                    alice_session = alice.register(
                        "alice", "pw-alice-1", "Alice"
                    )
                    bob_session = bob.register("bob", "pw-bob-1", "Bob")
                    self.assertIsNotNone(alice_session)
                    self.assertIsNotNone(bob_session)
                    assert alice_session is not None and bob_session is not None
                    self.assertTrue(alice_session.active)
                    self.assertTrue(bob_session.active)
                    self.assertIsInstance(alice_session.avatar, AvatarInfo)
                    assert alice_session.avatar is not None
                    self.assertEqual(alice_session.avatar.initials, "AL")
                    self.assertEqual(alice_session["avatar"]["initials"], "AL")
                    self.assertIn(alice_session.avatar.color_index, range(8))
                    self.assertRegex(alice_session.avatar.color_hex, r"^#[0-9A-F]{6}$")

                    chat = alice.create_direct_chat("bob", e2e=True)
                    self.assertIsInstance(chat, dict)
                    self.assertEqual(chat.kind, "direct")
                    self.assertEqual(chat["kind"], "direct")
                    self.assertEqual(chat.e2e_mode, "olm_v1")
                    self.assertIsInstance(chat.avatar, AvatarInfo)
                    assert chat.avatar is not None
                    self.assertEqual(chat.avatar.initials, "BO")
                    self.assertEqual(chat["avatar"]["initials"], "BO")
                    self.assertIn(chat.avatar.color_index, range(8))
                    chat_id = chat.chat_id

                    local_id = alice.send_message(chat_id, text)
                    self.assertTrue(local_id)

                    deadline = time.monotonic() + 20.0
                    received = None
                    while time.monotonic() < deadline:
                        event = bob.poll_event(timeout=0.5)
                        if event is None:
                            continue
                        self.assertIsInstance(event, Event)
                        if (
                            event.event == "message_received"
                            and event.data.get("body") == text
                        ):
                            received = event
                            break
                    self.assertIsNotNone(
                        received,
                        "Bob did not receive message_received within 20s",
                    )
                    assert received is not None
                    self.assertEqual(received["event"], "message_received")
                    self.assertEqual(received.event, "message_received")

                    messages = bob.get_messages(chat_id)
                    received_message = next(
                        (
                            message
                            for message in messages
                            if isinstance(message, Message)
                            and message.body == text
                        ),
                        None,
                    )
                    self.assertIsNotNone(
                        received_message,
                        "message is absent from Bob's local history",
                    )
                    assert received_message is not None
                    # FFI пока не прикладывает avatar к сообщению; модель
                    # поддерживает поле для будущих версий.
                    self.assertIsNone(received_message.avatar)

                self.assertTrue(alice.closed)
                self.assertTrue(bob.closed)
                self.assertIsNone(alice.poll_event(timeout=0))
                self.assertIsNone(bob.poll_event(timeout=0))
                self.assertIsNone(
                    process.poll(),
                    "rchat-server crashed while the Python clients ran",
                )
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                log_file.close()
                if process.returncode not in (0, -15):
                    details = log_path.read_text(
                        encoding="utf-8", errors="replace"
                    )
                    if details:
                        print(details, file=sys.stderr)


if __name__ == "__main__":
    unittest.main()

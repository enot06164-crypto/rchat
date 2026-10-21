"""Тест мультипрофильного шлюза: один процесс обслуживает несколько
аккаунтов, профиль выбирается при подключении (``/ws?profile=<имя>``).

Проверяем:
- список профилей (``/ws?list=profiles``);
- разные профили — разные сессии (своя БД, свои секреты);
- одно и то же имя профиля из разных подключений — одна и та же сессия;
- некорректное имя профиля отвергается (имя становится элементом пути).
"""

from __future__ import annotations

import json
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from websockets.sync.client import connect  # noqa: E402

from rchat import ffi  # noqa: E402

ROOT = Path(__file__).resolve().parents[3]


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
            raise RuntimeError(f"gateway exited with {process.returncode}")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError("gateway did not start listening")


def _call(ws: object, request: dict[str, object]) -> dict[str, object]:
    ws.send(json.dumps(request))  # type: ignore[attr-defined]
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        message = json.loads(ws.recv(timeout=5))  # type: ignore[attr-defined]
        if message.get("@type") == "event":
            continue
        return message
    raise AssertionError("no answer from the gateway")


class GatewayProfilesTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        try:
            ffi.get_library()
        except Exception as error:  # noqa: BLE001 - test bootstrap
            raise RuntimeError(
                "rchat FFI library is not available; run "
                "'cargo build -p rchat-ffi' first"
            ) from error

    def test_gateway_serves_several_profiles(self) -> None:
        with tempfile.TemporaryDirectory(prefix="rchat-gateway-profiles-") as root:
            root_path = Path(root)
            port = _free_port()
            process = subprocess.Popen(
                [
                    sys.executable,
                    "-m",
                    "rchat.gateway",
                    "--data-dir",
                    str(root_path),
                    "--server",
                    "ws://127.0.0.1:1",  # сервер не нужен: регистрация не нужна
                    "--port",
                    str(port),
                    # Личный режим: профили выбираются по имени.
                    "--single-user",
                ],
                cwd=ROOT,
                env={
                    **dict(__import__("os").environ),
                    "PYTHONPATH": str(ROOT / "bindings/python"),
                },
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            try:
                _wait_until_listening(process, port)
                url = f"ws://127.0.0.1:{port}/ws"

                # Список профилей: короткое подключение без профиля.
                with connect(f"{url}?list=profiles") as ws:
                    answer = json.loads(ws.recv(timeout=5))
                self.assertEqual(answer["@type"], "ok")
                self.assertEqual(answer["request"], "list_profiles")
                profiles = answer["data"]["profiles"]
                self.assertIn("default", profiles)

                # Профиль `default` — корень каталога данных.
                with connect(url) as ws:
                    answer = _call(ws, {"@type": "get_connection_state"})
                self.assertEqual(answer["@type"], "ok")
                self.assertTrue((root_path / "client.sqlite3").is_file())

                # Именованный профиль — отдельная БД в accounts/<имя>.
                with connect(f"{url}?profile=alice") as ws:
                    answer = _call(ws, {"@type": "get_connection_state"})
                self.assertEqual(answer["@type"], "ok")
                self.assertTrue(
                    (root_path / "accounts" / "alice" / "client.sqlite3").is_file()
                )

                # Профили не смешиваются: у каждого своя сессия.
                with connect(f"{url}?profile=alice") as ws:
                    alice = _call(ws, {"@type": "get_session"})
                with connect(f"{url}?profile=bob") as ws:
                    bob = _call(ws, {"@type": "get_session"})
                self.assertIsNone(alice["data"])
                self.assertIsNone(bob["data"])

                # Некорректное имя профиля (элемент пути) отвергается:
                # шлюз отвечает ошибкой и закрывает соединение.
                with connect(f"{url}?profile=../evil") as ws:
                    try:
                        answer = json.loads(ws.recv(timeout=5))
                    except Exception:  # noqa: BLE001 - закрыто без ответа
                        answer = None
                if answer is not None:
                    self.assertEqual(answer["@type"], "error")
                    self.assertIn("profile", answer["message"])
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


def _start_gateway(root: Path, port: int, *extra: str) -> subprocess.Popen[bytes]:
    process = subprocess.Popen(
        [
            sys.executable,
            "-m",
            "rchat.gateway",
            "--data-dir",
            str(root),
            "--server",
            "ws://127.0.0.1:1",  # сервер не нужен: вход не выполняется
            "--port",
            str(port),
            *extra,
        ],
        cwd=ROOT,
        env={
            **dict(__import__("os").environ),
            "PYTHONPATH": str(ROOT / "bindings/python"),
        },
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    _wait_until_listening(process, port)
    return process


def _stop(process: subprocess.Popen[bytes]) -> None:
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


class GatewayMultiuserTests(unittest.TestCase):
    """``--multiuser``: профиль по токену устройства, а не по имени."""

    @classmethod
    def setUpClass(cls) -> None:
        try:
            ffi.get_library()
        except Exception as error:  # noqa: BLE001 - test bootstrap
            raise RuntimeError(
                "rchat FFI library is not available; run "
                "'cargo build -p rchat-ffi' first"
            ) from error

    def test_device_token_isolates_profiles(self) -> None:
        with tempfile.TemporaryDirectory(prefix="rchat-gateway-multiuser-") as root:
            root_path = Path(root)
            port = _free_port()
            # Мультипользовательский режим — по умолчанию (без флагов).
            process = _start_gateway(root_path, port)
            url = f"ws://127.0.0.1:{port}/ws"
            device_a = "a" * 64
            device_b = "b" * 64
            try:
                # Без токена или с чужим `profile=` подключение отвергается:
                # профиль `default` наружу не отдаётся.
                for path in (url, f"{url}?profile=default", f"{url}?device=short"):
                    with connect(path) as ws:
                        answer = json.loads(ws.recv(timeout=5))
                    self.assertEqual(answer["@type"], "error")
                    self.assertIn("device", answer["message"])

                # Каждому устройству — свой профиль; список профилей —
                # только свой (чужие не раскрываются).
                with connect(f"{url}?list=profiles&device={device_a}") as ws:
                    listed = json.loads(ws.recv(timeout=5))
                with connect(f"{url}?list=profiles&device={device_b}") as ws:
                    listed_b = json.loads(ws.recv(timeout=5))
                self.assertTrue(listed["data"]["multiuser"])
                self.assertTrue(listed_b["data"]["multiuser"])
                profile_a = listed["data"]["profiles"][0]
                profile_b = listed_b["data"]["profiles"][0]
                self.assertNotEqual(profile_a, profile_b)
                self.assertTrue(profile_a.startswith("web-"))
                self.assertNotIn("default", listed["data"]["profiles"])

                # Устройства получают изолированные ядра: разные БД на диске,
                # ни у одного нет сессии.
                with connect(f"{url}?device={device_a}") as ws:
                    a_session = _call(ws, {"@type": "get_session"})
                with connect(f"{url}?device={device_b}") as ws:
                    b_session = _call(ws, {"@type": "get_session"})
                self.assertIsNone(a_session["data"])
                self.assertIsNone(b_session["data"])
                self.assertTrue(
                    (root_path / "accounts" / profile_a / "client.sqlite3").is_file()
                )
                self.assertTrue(
                    (root_path / "accounts" / profile_b / "client.sqlite3").is_file()
                )
                # Корень данных (`default`) в этом режиме не используется.
                self.assertFalse((root_path / "client.sqlite3").exists())

                # Тот же токен — тот же профиль (сеанс постоянный).
                with connect(f"{url}?device={device_a}") as ws:
                    again = _call(ws, {"@type": "get_connection_state"})
                self.assertEqual(again["@type"], "ok")
            finally:
                _stop(process)


if __name__ == "__main__":
    unittest.main()

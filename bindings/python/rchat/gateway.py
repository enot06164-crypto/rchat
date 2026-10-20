"""WebSocket-мост между браузером и ядром rchat (этап H).

Процесс шлюза обслуживает **несколько профилей** (аккаунтов): каждый
профиль — свой :class:`rchat.RChatClient` (своя БД, свои секреты, своё
E2E-состояние), профиль выбирается при подключении: ``/ws?profile=<имя>``.
Раскладка каталогов та же, что у CLI: ``--data-dir`` — корень профилей,
профиль ``default`` — сам корень, остальные — ``<корень>/accounts/<имя>``.
Список профилей шлюз отдаёт по ``GET /profiles``.

Браузер говорит на протоколе FFI без изменений
(``docs/ffi.md``):

* каждый текстовый кадр браузера проксируется в ``rchat_send_json``, ответ
  (``ok``/``error``) возвращается в этот же сокет;
* события (очередь ядра) ретранслируются всем подключённым вкладкам.

Пример:

.. code-block:: bash

   # терминал 1: сервер разработки
   cargo run --bin rchat-server -- \\
       --db /tmp/rchat-web/server.sqlite3 \\
       --listen 127.0.0.1:9000 \\
       --plaintext-insecure --allow-dev-plaintext-chats

   # терминал 2: шлюз.  Команды ядра блокирующие, поэтому сетевой цикл
   # вынесен в библиотеку websockets (внешний пакет, не stdlib):
   #     pip install websockets      (или uv pip install websockets)
   # --data-dir — корень профилей: профиль «default» — сам корень,
   # остальные — в <корень>/accounts/<имя> (как у CLI --account).
   PYTHONPATH=bindings/python python3 -m rchat.gateway \
       --data-dir /tmp/rchat-web \
       --server ws://127.0.0.1:9000 \
       --port 8080

Библиотека ``websockets`` — внешний пакет; для SDK он не нужен.  Тестирован
``websockets 17.2``; требуется ``websockets >= 12`` (API asyncio-сервера).
"""

from __future__ import annotations

import argparse
import asyncio
import json
import logging
import os
from typing import Any, Optional

_LOG = logging.getLogger("rchat.gateway")


# ---------------------------------------------------------------------------
# Bridge


class WebSocketBridge:
    """Owns one ``RChatClient`` and fans out its events."""

    def __init__(
        self,
        data_dir: str,
        server: str,
        e2e: bool = True,
        tls_fingerprint: Optional[str] = None,
        tls_insecure: bool = False,
        auto_start: bool = True,
        profile: str = "default",
    ) -> None:
        from rchat import RChatClient

        self.profile = profile
        self.data_dir = data_dir
        os.makedirs(data_dir, exist_ok=True)
        self._client = RChatClient(
            data_dir,
            server,
            e2e=e2e,
            tls_fingerprint=tls_fingerprint,
            tls_insecure=tls_insecure,
            auto_start=auto_start,
        )
        self.connections: set[Any] = set()
        self._closed = False

    def close(self) -> None:
        self._closed = True
        self._client.close()

    # -- requests -----------------------------------------------------------

    def dispatch_client_json(self, raw: str) -> dict[str, Any]:
        """Turn one raw browser request into an FFI-shaped response dict."""
        try:
            request = json.loads(raw)
        except ValueError as error:
            return _error_response("invalid_request", f"invalid JSON: {error}")
        if not isinstance(request, dict):
            return _error_response("invalid_request", "request must be a JSON object")
        method = request.get("@type")
        if not isinstance(method, str) or not method:
            return _error_response("invalid_request", "missing @type")
        try:
            data = self._client._command(request)  # noqa: SLF001 same package
        except BaseException as error:  # noqa: BLE001 - rendered as an FFI error
            return _error_from_exception(error, method)
        return {"@type": "ok", "request": method, "data": data}

    # -- events ---------------------------------------------------------------

    async def relay_loop(self) -> None:
        """Push native events onto all current sockets (loop-thread only)."""
        loop = asyncio.get_running_loop()
        while not self._closed:
            try:
                event = await loop.run_in_executor(
                    None, self._client.poll_event, 0.25
                )
            except asyncio.CancelledError:
                raise
            except RuntimeError:
                # The loop is shutting down (executor closed).  Leave quietly.
                if self._closed:
                    return
                _LOG.exception("event relay executor is not running; retrying")
                await asyncio.sleep(0.5)
            except Exception as error:  # noqa: BLE001 - stats only
                if self._closed:
                    return
                _LOG.exception("event poll failed: %s", error)
                await asyncio.sleep(0.5)
            if event is None:
                continue
            if not self.connections:
                continue
            payload = json.dumps(
                {"@type": "event", "event": event.event, "data": event.data}
            )
            from websockets.asyncio.server import broadcast

            broadcast(self.connections, payload, raise_exceptions=True)


# ---------------------------------------------------------------------------
# Профили (аккаунты)


#: Имя профиля по умолчанию — корень каталога данных.
DEFAULT_PROFILE = "default"

#: Допустимое имя профиля: буквы, цифры, дефис и подчёркивание.  Имя
#: становится элементом пути, поэтому произвольные строки не принимаются.
_PROFILE_CHARS = set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_")


def valid_profile(name: str) -> bool:
    """Проверяет имя профиля (путь, регистр, длина)."""
    return 1 <= len(name) <= 32 and all(char in _PROFILE_CHARS for char in name)


class ProfileRegistry:
    """Профили аккаунтов: по одному мосту (:class:`WebSocketBridge`) на профиль.

    Мосты создаются лениво (при первом подключении к профилю) и живут до
    конца работы шлюза: у каждого профиля свои подключения, свои события и
    своё состояние ядра.  Раскладка каталогов — как у CLI: профиль
    ``default`` — сам корень ``data_root``, остальные — в
    ``<data_root>/accounts/<имя>``.
    """

    def __init__(
        self,
        data_root: str,
        server: str,
        e2e: bool = True,
        tls_fingerprint: Optional[str] = None,
        tls_insecure: bool = False,
        auto_start: bool = True,
    ) -> None:
        self.data_root = os.path.abspath(data_root)
        self._server = server
        self._e2e = e2e
        self._tls_fingerprint = tls_fingerprint
        self._tls_insecure = tls_insecure
        self._auto_start = auto_start
        self._bridges: dict[str, WebSocketBridge] = {}
        self._relays: list[asyncio.Task[None]] = []

    # -- профили ------------------------------------------------------------

    def profile_dir(self, profile: str) -> str:
        """Каталог профиля."""
        if profile == DEFAULT_PROFILE:
            return self.data_root
        return os.path.join(self.data_root, "accounts", profile)

    def list_profiles(self) -> list[str]:
        """Известные профили: ``default`` и каталоги в ``accounts/``."""
        profiles: list[str] = [DEFAULT_PROFILE]
        accounts_dir = os.path.join(self.data_root, "accounts")
        try:
            names = sorted(os.listdir(accounts_dir))
        except OSError:
            names = []
        for name in names:
            if not valid_profile(name):
                continue
            if os.path.isdir(os.path.join(accounts_dir, name)):
                profiles.append(name)
        return profiles

    # -- мосты --------------------------------------------------------------

    def bridge(self, profile: str) -> WebSocketBridge:
        """Мост профиля (создаётся при первом обращении)."""
        bridge = self._bridges.get(profile)
        if bridge is None:
            bridge = WebSocketBridge(
                self.profile_dir(profile),
                self._server,
                e2e=self._e2e,
                tls_fingerprint=self._tls_fingerprint,
                tls_insecure=self._tls_insecure,
                auto_start=self._auto_start,
                profile=profile,
            )
            self._bridges[profile] = bridge
            self._relays.append(asyncio.create_task(bridge.relay_loop()))
            _LOG.info("profile %s opened at %s", profile, bridge.data_dir)
        return bridge

    def close(self) -> None:
        """Закрывает все профили."""
        for relay in self._relays:
            relay.cancel()
        self._relays.clear()
        for bridge in self._bridges.values():
            bridge.close()
        self._bridges.clear()


def _error_response(code: str, message: str, method: Optional[str] = None) -> dict[str, Any]:
    response: dict[str, Any] = {
        "@type": "error",
        "code": "invalid_request" if code == "invalid_request" else code,
        "message": message,
        "retryable": False,
    }
    if method:
        response["request"] = method
    return response


def _error_from_exception(error: BaseException, method: str) -> dict[str, Any]:
    from rchat.exceptions import RChatError

    if not isinstance(error, RChatError):
        _LOG.exception("unexpected gateway error", exc_info=error)
        response = _error_response("internal", str(error), method)
        return response
    retryable = "true" if getattr(error, "retryable", False) else "false"
    response: dict[str, Any] = {
        "@type": "error",
        "request": method,
        "code": error.code,
        "message": error.message,
        "retryable": retryable == "true",
    }
    server_code = getattr(error, "server_code", None)
    if server_code is not None:
        response["server_code"] = server_code
        name = getattr(error, "server_code_name", None)
        if name:
            response["server_code_name"] = name
    return response


# ---------------------------------------------------------------------------
# websocket server


def _wants_profile_list(path: str) -> bool:
    """Запрошен ли список профилей (``/ws?list=profiles``)."""
    from urllib.parse import parse_qs, urlsplit

    query = parse_qs(urlsplit(path).query)
    values = query.get("list") or []
    return bool(values) and values[0] == "profiles"


def _profile_from_path(path: str) -> Optional[str]:
    """Имя профиля из пути соединения (``/ws?profile=alice``)."""
    from urllib.parse import parse_qs, urlsplit

    query = parse_qs(urlsplit(path).query)
    values = query.get("profile") or []
    if not values:
        return DEFAULT_PROFILE
    name = values[0].strip()
    if not valid_profile(name):
        return None
    return name


def _make_process_request() -> Any:
    """Ответ на обычный HTTP-запрос: 404 (статику отдаёт Vite)."""

    def process_request(connection: Any, request: Any) -> Any:
        from urllib.parse import urlsplit

        if urlsplit(request.path).path == "/ws":
            return None
        return connection.respond(
            404,
            '{"error":"no HTTP here; use /ws or run `npm run dev` in apps/web"}',
        )

    return process_request


async def _handler(registry: ProfileRegistry, connection: Any) -> None:
    from websockets.exceptions import ConnectionClosed  # type: ignore[import-not-found]

    # Список профилей — короткое подключение без профиля: `/ws?list=profiles`.
    if _wants_profile_list(connection.request.path):
        await connection.send(
            json.dumps(
                {
                    "@type": "ok",
                    "request": "list_profiles",
                    "data": {"profiles": registry.list_profiles()},
                }
            )
        )
        await connection.close()
        return
    profile = _profile_from_path(connection.request.path)
    if profile is None:
        await connection.send(
            json.dumps(
                _error_response(
                    "invalid_request",
                    "invalid profile name (letters, digits, - and _ up to 32 chars)",
                )
            )
        )
        await connection.close()
        return
    bridge = registry.bridge(profile)
    _LOG.info(
        "client %s connected (profile %s)", connection.remote_address, profile
    )
    bridge.connections.add(connection)
    try:
        async for raw in connection:
            if isinstance(raw, bytes):  # frame was binary
                await connection.send(
                    json.dumps(
                        _error_response(
                            "invalid_request", "only text frames are supported"
                        )
                    )
                )
                continue
            response = bridge.dispatch_client_json(raw)
            await connection.send(json.dumps(response))
    except ConnectionClosed:
        pass
    finally:
        bridge.connections.discard(connection)
        _LOG.info(
            "client %s left (profile %s)", connection.remote_address, profile
        )


async def _serve(registry: ProfileRegistry, host: str, port: int) -> None:
    import signal

    from websockets.asyncio.server import serve

    async def handler(connection: Any) -> None:
        await _handler(registry, connection)

    def handle_sigint() -> None:
        raise KeyboardInterrupt

    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        try:
            loop.add_signal_handler(sig, handle_sigint)
        except NotImplementedError:  # pragma: no cover - windows
            pass

    async with serve(
        handler,
        host,
        port,
        process_request=_make_process_request(),
        max_size=1 << 20,
    ) as server:
        _LOG.info(
            "rchat websocket gateway on ws://%s:%d/ws (profiles: %s)",
            host,
            port,
            ", ".join(registry.list_profiles()),
        )
        try:
            await server.serve_forever()
        except (asyncio.CancelledError, KeyboardInterrupt):
            pass


# ---------------------------------------------------------------------------
# CLI


def _parse_args(argv: Optional[list[str]] = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog="rchat.gateway",
        description="local WebSocket bridge between the browser and rchat-ffi",
    )
    parser.add_argument(
        "--data-dir",
        required=True,
        help=(
            "root directory of client profiles: profile 'default' is the root "
            "itself, others live in <root>/accounts/<name> (own SQLite, "
            "secrets, E2E state)"
        ),
    )
    parser.add_argument(
        "--server",
        required=True,
        help="rchat server URL, e.g. ws://127.0.0.1:9000 (dev) or wss://host",
    )
    parser.add_argument(
        "--host",
        default="127.0.0.1",
        help="bind address for the local WebSocket server (default 127.0.0.1)",
    )
    parser.add_argument(
        "--port",
        type=int,
        default=8080,
        help="local WebSocket port (default 8080, browser path /ws)",
    )
    parser.add_argument(
        "--e2e",
        dest="e2e",
        default=True,
        action=argparse.BooleanOptionalAction,
        help="enable E2E chats for the profile (default on)",
    )
    parser.add_argument(
        "--tls-fingerprint",
        default=None,
        metavar="SHA256",
        help="pin the server TLS fingerprint for wss:// (AA:BB:... / hex)",
    )
    parser.add_argument(
        "--tls-insecure",
        default=False,
        action="store_true",
        help="skip TLS certificate verification (dev only)",
    )
    return parser.parse_args(argv)


def main(argv: Optional[list[str]] = None) -> int:
    logging.basicConfig(
        level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s: %(message)s"
    )
    args = _parse_args(argv)
    try:
        import websockets  # noqa: F401 - fail early with a clear message
    except ModuleNotFoundError as error:
        _LOG.error(
            "the gateway needs the standalone `websockets` package "
            "(uv pip install websockets or pip install websockets): %s",
            error,
        )
        return 2

    registry = ProfileRegistry(
        data_root=args.data_dir,
        server=args.server,
        e2e=args.e2e,
        tls_fingerprint=args.tls_fingerprint,
        tls_insecure=args.tls_insecure,
    )
    try:
        asyncio.run(_serve(registry, args.host, args.port))
    except KeyboardInterrupt:
        pass
    finally:
        registry.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

"""High-level, thread-safe Python client for ``rchat-ffi``."""

from __future__ import annotations

import queue
import threading
import time
from collections.abc import Mapping as MappingABC
from pathlib import Path
from typing import Any, Dict, Generator, List, Mapping, Optional, Sequence, Union

from . import ffi
from .exceptions import (
    RChatClosedError,
    RChatConfigError,
    RChatError,
    RChatException,
)
from .models import Call, Chat, Event, Message, Session

PathLike = Union[str, Path]


def _one(model: Any, data: Any, what: str) -> Any:
    if not isinstance(data, MappingABC):
        raise RChatError("internal", f"invalid {what} payload: {data!r}")
    try:
        return model.from_dict(data)
    except (KeyError, TypeError, ValueError) as error:
        raise RChatError("internal", f"invalid {what} payload: {error}") from error


def _many(model: Any, data: Any, what: str) -> List[Any]:
    if not isinstance(data, list):
        raise RChatError("internal", f"invalid {what} list payload: {data!r}")
    result = []
    for item in data:
        result.append(_one(model, item, what))
    return result


class RChatClient:
    """An independent rchat profile with a background event reader.

    The constructor creates one native handle and starts a daemon thread that
    drains ``rchat_receive_json`` into an unbounded Python queue.  Use
    :meth:`poll_event` or :meth:`events` to consume events.

    ``close()`` (or the context-manager protocol) stops the reader before
    destroying the native handle, so it is safe to call from normal user code.
    """

    def __init__(
        self,
        data_dir: PathLike,
        server: str,
        e2e: bool = True,
        tls_fingerprint: Optional[str] = None,
        tls_insecure: bool = False,
        auto_start: bool = True,
    ) -> None:
        self._data_dir = str(data_dir)
        self._server = server
        self._e2e = bool(e2e)
        self._tls_fingerprint = tls_fingerprint
        self._tls_insecure = bool(tls_insecure)
        self._auto_start = bool(auto_start)

        config: Dict[str, Any] = {
            "data_dir": self._data_dir,
            "server": server,
            "e2e": self._e2e,
            "auto_start": self._auto_start,
        }
        if tls_fingerprint is not None:
            config["tls_fingerprint"] = tls_fingerprint
        if self._tls_insecure:
            config["tls_insecure"] = True

        handle = ffi.rchat_create(config)
        if handle is None:
            message = ffi.rchat_last_error() or "unknown configuration error"
            raise RChatConfigError(f"rchat_create failed: {message}")

        self._handle: Optional[int] = handle
        self._closed = False
        self._stop_event = threading.Event()
        self._state_lock = threading.Lock()
        self._command_lock = threading.Lock()
        self._events: "queue.Queue[Event]" = queue.Queue()
        self._event_error: Optional[BaseException] = None
        try:
            self._thread = threading.Thread(
                target=self._event_loop,
                name=f"rchat-events-{id(self):x}",
                daemon=True,
            )
            self._thread.start()
        except BaseException:
            self._handle = None
            self._closed = True
            ffi.rchat_destroy(handle)
            raise

    # ------------------------------------------------------------------
    # Lifecycle

    @property
    def data_dir(self) -> str:
        return self._data_dir

    @property
    def server(self) -> str:
        return self._server

    @property
    def closed(self) -> bool:
        return self._closed

    def __enter__(self) -> "RChatClient":
        return self

    def __exit__(self, exc_type: Any, exc: Any, traceback: Any) -> None:
        self.close()

    def close(self) -> None:
        """Stop the event reader and destroy the native handle."""
        if threading.current_thread() is self._thread:
            raise RChatClosedError(
                "close() cannot be called from the event reader thread"
            )

        with self._state_lock:
            if self._closed:
                return
            self._closed = True
            thread = self._thread
            self._stop_event.set()

        # A receive call has a 500 ms timeout, so this join is bounded in a
        # healthy library.  Joining before destroy prevents use-after-free.
        if thread is not None:
            thread.join()

        # Events are no longer observable once the client is closed; dropping
        # them also releases message bodies promptly.
        while True:
            try:
                self._events.get_nowait()
            except queue.Empty:
                break

        with self._command_lock:
            handle = self._handle
            self._handle = None
        if handle is not None:
            ffi.rchat_destroy(handle)

    def _ensure_open(self) -> int:
        with self._state_lock:
            if self._closed or self._handle is None:
                raise RChatClosedError("rchat client is closed")
            return self._handle

    def _command(self, request: Mapping[str, Any]) -> Any:
        with self._command_lock:
            handle = self._ensure_open()
            response = ffi.rchat_send_json(handle, dict(request))
            if response.get("@type") == "error":
                raise RChatError.from_response(response)
            if response.get("@type") != "ok":
                raise RChatError(
                    "internal",
                    f"unexpected response from {request.get('@type')}: {response!r}",
                )
            return response.get("data")

    # ------------------------------------------------------------------
    # Event stream

    def _event_loop(self) -> None:
        handle = self._handle
        try:
            while not self._stop_event.is_set():
                raw = ffi.rchat_receive_json(handle, 500)
                if raw is None:
                    continue
                try:
                    event = Event.from_dict(raw)
                except Exception:
                    event = Event(event="malformed_event", data={"raw": raw})
                self._events.put(event)
        except BaseException as error:  # noqa: BLE001 - surfaced to consumers
            self._event_error = error

    def _check_event_error(self) -> None:
        error = self._event_error
        if error is None or not self._events.empty():
            return
        self._event_error = None
        if isinstance(error, RChatException):
            raise error
        raise RChatException(f"event reader failed: {error}") from error

    def poll_event(self, timeout: Optional[float] = None) -> Optional[Event]:
        """Return the next event, or ``None`` after a timeout/close.

        ``timeout`` is in seconds.  ``None`` waits indefinitely (but wakes up
        periodically to notice a closed client or a failed reader thread).
        """
        if timeout is not None and timeout < 0:
            raise ValueError("timeout must be non-negative")
        deadline = None if timeout is None else time.monotonic() + timeout

        while True:
            if self._closed:
                return None
            if deadline is None:
                wait = 0.25
            else:
                remaining = deadline - time.monotonic()
                wait = max(0.0, min(remaining, 0.25))
            try:
                return self._events.get(timeout=wait)
            except queue.Empty:
                self._check_event_error()
                if self._closed:
                    return None
                if deadline is not None and time.monotonic() >= deadline:
                    return None

    def events(self) -> Generator[Event, None, None]:
        """Yield events until the client is closed."""
        while True:
            event = self.poll_event(timeout=0.5)
            if event is not None:
                yield event
            elif self._closed:
                return

    # ------------------------------------------------------------------
    # Session and connection

    def ping(self) -> Dict[str, Any]:
        return self._command({"@type": "ping"})

    def start(self) -> Dict[str, Any]:
        return self._command({"@type": "start"})

    def get_connection_state(self) -> Dict[str, Any]:
        return self._command({"@type": "get_connection_state"})

    def wait_online(self, timeout_ms: int = 5000) -> Dict[str, Any]:
        return self._command(
            {"@type": "wait_online", "timeout_ms": int(timeout_ms)}
        )

    def get_session(self) -> Optional[Session]:
        data = self._command({"@type": "get_session"})
        return None if data is None else _one(Session, data, "session")

    def get_me(self) -> Optional[Session]:
        """Return the locally known account/session, if any."""
        return self.get_session()

    def get_account(self) -> Optional[Dict[str, Any]]:
        data = self._command({"@type": "get_account"})
        return data if isinstance(data, dict) else None

    def get_status(self) -> Dict[str, Any]:
        return self._command({"@type": "get_status"})

    def register(
        self,
        username: str,
        password: str,
        display_name: Optional[str] = None,
    ) -> Optional[Session]:
        """Create an account and authenticate this profile.

        Like the native command, this fails with ``invalid_request`` if the
        profile already has an active session.
        """
        request: Dict[str, Any] = {
            "@type": "register",
            "username": username,
            "password": password,
        }
        if display_name is not None:
            request["display_name"] = display_name
        data = self._command(request)
        return None if data is None else _one(Session, data, "session")

    def login(self, username: str, password: str) -> Optional[Session]:
        """Authenticate a profile that has no active session.

        A profile with a saved token is restored automatically when the actor
        starts; calling ``login`` for it returns ``invalid_request`` with the
        message ``already authenticated``.  Use :meth:`get_me` or
        :meth:`get_session` to reuse the restored session.
        """
        data = self._command(
            {"@type": "login", "username": username, "password": password}
        )
        return None if data is None else _one(Session, data, "session")

    def logout(self, wipe: bool = False) -> Dict[str, Any]:
        return self._command({"@type": "logout", "wipe": bool(wipe)})

    # ------------------------------------------------------------------
    # Chats

    def get_chats(self) -> List[Chat]:
        return _many(Chat, self._command({"@type": "get_chats"}), "chat")

    def refresh_chats(self) -> List[Chat]:
        return _many(
            Chat, self._command({"@type": "refresh_chats"}), "chat"
        )

    def get_chat_members(self, chat_id: str) -> List[Dict[str, Any]]:
        data = self._command(
            {"@type": "get_chat_members", "chat_id": chat_id}
        )
        if not isinstance(data, list):
            raise RChatError("internal", f"invalid members payload: {data!r}")
        return [item for item in data if isinstance(item, dict)]

    def create_direct_chat(
        self, peer_username: str, e2e: bool = True
    ) -> Chat:
        data = self._command(
            {
                "@type": "create_direct_chat",
                "peer_username": peer_username,
                "e2e": bool(e2e),
            }
        )
        return _one(Chat, data, "chat")

    def create_group_chat(
        self,
        name: str,
        members: Optional[Sequence[str]] = None,
        e2e: bool = True,
    ) -> Chat:
        request: Dict[str, Any] = {
            "@type": "create_group_chat",
            "name": name,
            "e2e": bool(e2e),
        }
        if members is not None:
            request["members"] = list(members)
        data = self._command(request)
        return _one(Chat, data, "chat")

    def invite_to_chat(
        self, chat_id: str, usernames: Sequence[str]
    ) -> Chat:
        data = self._command(
            {
                "@type": "invite_to_chat",
                "chat_id": chat_id,
                "usernames": list(usernames),
            }
        )
        return _one(Chat, data, "chat")

    def leave_chat(self, chat_id: str) -> None:
        self._command({"@type": "leave_chat", "chat_id": chat_id})

    def remove_chat_member(self, chat_id: str, user_id: str) -> Chat:
        data = self._command(
            {
                "@type": "remove_chat_member",
                "chat_id": chat_id,
                "user_id": user_id,
            }
        )
        return _one(Chat, data, "chat")

    def rename_chat(self, chat_id: str, name: str) -> Chat:
        data = self._command(
            {"@type": "rename_chat", "chat_id": chat_id, "name": name}
        )
        return _one(Chat, data, "chat")

    # ------------------------------------------------------------------
    # Messages, history and receipts

    def send_message(
        self,
        chat_id: str,
        text: str = "",
        reply_to_server_id: Optional[int] = None,
        attachment: Optional[Dict[str, Any]] = None,
    ) -> str:
        """Отправляет сообщение; `attachment` — вложение (см. docs/ffi.md §5.4).

        Вложение едет внутри зашифрованного payload: сервер не видит его
        содержимое. Формат: `{"kind", "name", "mime", "size", "data_base64"}`.
        """
        request: Dict[str, Any] = {
            "@type": "send_message",
            "chat_id": chat_id,
            "text": text,
        }
        if reply_to_server_id is not None:
            request["reply_to_server_id"] = int(reply_to_server_id)
        if attachment is not None:
            request["attachment"] = attachment
        data = self._command(request)
        if not isinstance(data, dict) or not isinstance(
            data.get("local_id"), str
        ):
            raise RChatError("internal", f"invalid send response: {data!r}")
        return data["local_id"]

    def download_attachment(self, server_id: int) -> Dict[str, Any]:
        """Содержимое вложения сообщения: kind/name/mime/size/data_base64."""
        data = self._command(
            {"@type": "download_attachment", "server_id": int(server_id)}
        )
        if not isinstance(data, dict):
            raise RChatError("internal", f"invalid attachment response: {data!r}")
        return data

    def get_messages(
        self,
        chat_id: str,
        before_server_id: Optional[int] = None,
        limit: int = 50,
    ) -> List[Message]:
        request: Dict[str, Any] = {
            "@type": "get_messages",
            "chat_id": chat_id,
            "limit": int(limit),
        }
        if before_server_id is not None:
            request["before_server_id"] = int(before_server_id)
        return _many(Message, self._command(request), "message")

    def fetch_history(
        self,
        chat_id: str,
        before_server_id: Optional[int] = None,
        limit: int = 50,
    ) -> List[Message]:
        request: Dict[str, Any] = {
            "@type": "fetch_history",
            "chat_id": chat_id,
            "limit": int(limit),
        }
        if before_server_id is not None:
            request["before_server_id"] = int(before_server_id)
        return _many(Message, self._command(request), "message")

    def edit_message(
        self, chat_id: str, server_id: int, text: str
    ) -> Message:
        data = self._command(
            {
                "@type": "edit_message",
                "chat_id": chat_id,
                "server_id": int(server_id),
                "text": text,
            }
        )
        return _one(Message, data, "message")

    def delete_message(self, chat_id: str, server_id: int) -> Message:
        data = self._command(
            {
                "@type": "delete_message",
                "chat_id": chat_id,
                "server_id": int(server_id),
            }
        )
        return _one(Message, data, "message")

    def mark_read(
        self, chat_id: str, up_to_server_id: Optional[int] = None
    ) -> Dict[str, Any]:
        request: Dict[str, Any] = {
            "@type": "mark_read",
            "chat_id": chat_id,
        }
        if up_to_server_id is not None:
            request["up_to_server_id"] = int(up_to_server_id)
        return self._command(request)

    def get_receipts(self, chat_id: str) -> List[Dict[str, Any]]:
        data = self._command(
            {"@type": "get_receipts", "chat_id": chat_id}
        )
        if not isinstance(data, list):
            raise RChatError("internal", f"invalid receipts payload: {data!r}")
        return [item for item in data if isinstance(item, dict)]

    def sync(self) -> Dict[str, Any]:
        return self._command({"@type": "sync"})

    # ------------------------------------------------------------------
    # Devices

    def get_devices(self) -> List[Dict[str, Any]]:
        data = self._command({"@type": "get_devices"})
        if not isinstance(data, list):
            raise RChatError("internal", f"invalid devices payload: {data!r}")
        return [item for item in data if isinstance(item, dict)]

    def refresh_devices(self, user_ids: Sequence[str]) -> None:
        self._command(
            {"@type": "refresh_devices", "user_ids": list(user_ids)}
        )

    def revoke_device(self, device_id: str) -> None:
        self._command(
            {"@type": "revoke_device", "device_id": device_id}
        )

    # ------------------------------------------------------------------
    # Call signaling (no media/WebRTC)

    def call_invite(self, chat_id: str) -> Call:
        data = self._command(
            {"@type": "call_invite", "chat_id": chat_id}
        )
        return _one(Call, data, "call")

    def call_accept(self, call_id: str) -> Call:
        data = self._command(
            {"@type": "call_accept", "call_id": call_id}
        )
        return _one(Call, data, "call")

    def call_decline(self, call_id: str) -> Call:
        data = self._command(
            {"@type": "call_decline", "call_id": call_id}
        )
        return _one(Call, data, "call")

    def call_end(self, call_id: str) -> Call:
        data = self._command({"@type": "call_end", "call_id": call_id})
        return _one(Call, data, "call")

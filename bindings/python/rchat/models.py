"""Typed models returned by the high-level Python client.

Each model is a dataclass *and* a ``dict`` subclass.  That keeps the SDK
convenient (``message.body``) while remaining compatible with code written
against the raw JSON shape (``message["body"]``, ``json.dumps(message)``).
Unknown JSON fields are ignored for forward compatibility.
"""

from __future__ import annotations

from collections.abc import Mapping as MappingABC
from dataclasses import dataclass, fields
from typing import Any, Dict, Mapping, Optional


class _Model(dict):
    """Dataclass/dict bridge with a small amount of mapping convenience."""

    def __post_init__(self) -> None:
        dict.__init__(self)
        for field in fields(self):  # type: ignore[arg-type]
            dict.__setitem__(self, field.name, getattr(self, field.name))

    def to_dict(self) -> Dict[str, Any]:
        """Return a plain ``dict`` copy of the model."""
        return dict(self)

    as_dict = to_dict


@dataclass(eq=False)
class AvatarInfo(_Model):
    """Детерминированные инициалы и цвет аватара из FFI."""

    initials: str
    color_index: int
    color_hex: str

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> "AvatarInfo":
        return cls(
            initials=str(data.get("initials") or ""),
            color_index=int(data["color_index"]),
            color_hex=str(data["color_hex"]),
        )


def _avatar(value: Any) -> Optional[AvatarInfo]:
    """Разбирает необязательный объект avatar, не ломая старые ответы FFI."""
    if not isinstance(value, MappingABC):
        return None
    try:
        return AvatarInfo.from_dict(value)
    except (KeyError, TypeError, ValueError):
        return None


def _attachment(value: Any) -> Optional[Attachment]:
    """Разбирает необязательные метаданные вложения."""
    if not isinstance(value, MappingABC):
        return None
    try:
        return Attachment.from_dict(value)
    except (KeyError, TypeError, ValueError):
        return None


@dataclass(eq=False)
class Chat(_Model):
    chat_id: str
    kind: str
    e2e_mode: str
    title: str
    created_at_ms: int
    updated_at_ms: int
    last_server_id: int
    peer_user_id: Optional[str] = None
    avatar: Optional[AvatarInfo] = None

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> "Chat":
        return cls(
            chat_id=data["chat_id"],
            kind=data["kind"],
            e2e_mode=data["e2e_mode"],
            title=data["title"],
            created_at_ms=int(data["created_at_ms"]),
            updated_at_ms=int(data["updated_at_ms"]),
            last_server_id=int(data["last_server_id"]),
            peer_user_id=data.get("peer_user_id"),
            avatar=_avatar(data.get("avatar")),
        )


@dataclass(eq=False)
class Attachment(_Model):
    """Метаданные вложения сообщения (содержимое — `download_attachment`)."""

    kind: str
    name: str
    mime: str
    size: int

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> "Attachment":
        return cls(
            kind=str(data.get("kind") or "file"),
            name=str(data.get("name") or ""),
            mime=str(data.get("mime") or ""),
            size=int(data.get("size") or 0),
        )


@dataclass(eq=False)
class Message(_Model):
    local_id: str
    chat_id: str
    sender_user_id: str
    sender_device_id: str
    content_kind: str
    status: str
    created_at_ms: int
    server_id: Optional[int] = None
    body: Optional[str] = None
    attachment: Optional[Attachment] = None
    reply_to_server_id: Optional[int] = None
    edited_at_ms: Optional[int] = None
    deleted_at_ms: Optional[int] = None
    avatar: Optional[AvatarInfo] = None

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> "Message":
        return cls(
            local_id=data["local_id"],
            chat_id=data["chat_id"],
            sender_user_id=data["sender_user_id"],
            sender_device_id=data["sender_device_id"],
            content_kind=data["content_kind"],
            status=data["status"],
            created_at_ms=int(data["created_at_ms"]),
            server_id=data.get("server_id"),
            body=data.get("body"),
            attachment=_attachment(data.get("attachment")),
            reply_to_server_id=data.get("reply_to_server_id"),
            edited_at_ms=data.get("edited_at_ms"),
            deleted_at_ms=data.get("deleted_at_ms"),
            avatar=_avatar(data.get("avatar")),
        )


@dataclass(eq=False)
class Call(_Model):
    call_id: str
    chat_id: str
    caller_user_id: str
    callee_user_id: str
    state: str
    created_at_ms: int
    accepted_at_ms: Optional[int] = None

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> "Call":
        return cls(
            call_id=data["call_id"],
            chat_id=data["chat_id"],
            caller_user_id=data["caller_user_id"],
            callee_user_id=data["callee_user_id"],
            state=data["state"],
            created_at_ms=int(data["created_at_ms"]),
            accepted_at_ms=data.get("accepted_at_ms"),
        )


@dataclass(eq=False)
class Session(_Model):
    active: bool
    user_id: str
    username: str
    display_name: str
    device_id: Optional[str] = None
    avatar: Optional[AvatarInfo] = None

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> "Session":
        return cls(
            active=bool(data["active"]),
            user_id=data["user_id"],
            username=data["username"],
            display_name=data["display_name"],
            device_id=data.get("device_id"),
            avatar=_avatar(data.get("avatar")),
        )


@dataclass(eq=False)
class Event(_Model):
    event: str
    data: Dict[str, Any]

    def __post_init__(self) -> None:
        # Keep the raw event envelope available to mapping-style callers.
        dict.__init__(
            self,
            **{"@type": "event", "event": self.event, "data": self.data},
        )

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> "Event":
        payload = data.get("data")
        if not isinstance(payload, dict):
            payload = {}
        return cls(event=str(data.get("event") or ""), data=payload)

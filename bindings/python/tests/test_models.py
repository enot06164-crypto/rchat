"""Unit tests for dataclass/dict models, including avatar metadata."""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from rchat import AvatarInfo, Chat, Message, Session  # noqa: E402


AVATAR = {
    "initials": "АИ",
    "color_index": 5,
    "color_hex": "#65AADD",
}

CHAT = {
    "chat_id": "chat-1",
    "kind": "group",
    "e2e_mode": "olm_v1",
    "privacy": "private",
    "title": "Алиса Иванова",
    "peer_user_id": None,
    "created_at_ms": 1,
    "updated_at_ms": 2,
    "last_server_id": 3,
    "avatar": AVATAR,
}

SESSION = {
    "active": True,
    "user_id": "user-1",
    "username": "alice",
    "display_name": "Алиса Иванова",
    "device_id": "device-1",
    "avatar": AVATAR,
}

MESSAGE = {
    "local_id": "local-1",
    "server_id": 7,
    "chat_id": "chat-1",
    "sender_user_id": "user-1",
    "sender_device_id": "device-1",
    "content_kind": "text",
    "body": "привет",
    "reply_to_server_id": None,
    "status": "none",
    "created_at_ms": 10,
    "edited_at_ms": None,
    "deleted_at_ms": None,
    "avatar": AVATAR,
}


class AvatarModelTests(unittest.TestCase):
    def test_avatar_is_attribute_and_mapping(self) -> None:
        avatar = AvatarInfo.from_dict(AVATAR)
        self.assertEqual(avatar.initials, "АИ")
        self.assertEqual(avatar["color_index"], 5)
        self.assertEqual(avatar.color_hex, "#65AADD")
        self.assertEqual(avatar.to_dict(), AVATAR)
        self.assertEqual(json.loads(json.dumps(avatar)), AVATAR)

    def test_chat_embeds_avatar(self) -> None:
        chat = Chat.from_dict(CHAT)
        self.assertIsInstance(chat, dict)
        self.assertIsInstance(chat.avatar, AvatarInfo)
        self.assertEqual(chat.avatar.initials, "АИ")
        self.assertEqual(chat["avatar"]["initials"], "АИ")
        self.assertEqual(chat.avatar.color_hex, "#65AADD")
        self.assertEqual(json.loads(json.dumps(chat)), CHAT)

    def test_session_embeds_avatar(self) -> None:
        session = Session.from_dict(SESSION)
        self.assertIsInstance(session, dict)
        self.assertIsInstance(session.avatar, AvatarInfo)
        self.assertEqual(session.avatar.color_index, 5)
        self.assertEqual(session["avatar"]["color_hex"], "#65AADD")

    def test_message_avatar_is_optional(self) -> None:
        message = Message.from_dict(MESSAGE)
        self.assertIsInstance(message.avatar, AvatarInfo)
        self.assertEqual(message.avatar.initials, "АИ")
        self.assertEqual(message["avatar"]["initials"], "АИ")

        legacy = dict(MESSAGE)
        del legacy["avatar"]
        without = Message.from_dict(legacy)
        self.assertIsNone(without.avatar)
        self.assertIsNone(without["avatar"])

    def test_old_payloads_without_avatar_still_load(self) -> None:
        chat = dict(CHAT)
        del chat["avatar"]
        self.assertIsNone(Chat.from_dict(chat).avatar)

        session = dict(SESSION)
        del session["avatar"]
        self.assertIsNone(Session.from_dict(session).avatar)

    def test_malformed_avatar_is_ignored(self) -> None:
        chat = dict(CHAT)
        chat["avatar"] = {"initials": "АИ"}
        self.assertIsNone(Chat.from_dict(chat).avatar)


if __name__ == "__main__":
    unittest.main()

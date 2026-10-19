#!/usr/bin/env python3
"""Small echo bot built on the high-level rchat SDK."""

from __future__ import annotations

import argparse
import getpass
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from rchat import RChatClient, RChatException  # noqa: E402


def _password(args: argparse.Namespace) -> str:
    if args.password:
        return args.password
    from_env = os.environ.get("RCHAT_PASSWORD")
    if from_env:
        return from_env
    return getpass.getpass("password: ")


def _saved_session(
    client: "RChatClient", timeout: float = 2.0
):
    """Return the active session restored from the profile, if it appears."""
    if client.get_account() is None:
        return None
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        session = client.get_me()
        if session is not None and session.active:
            return session
        time.sleep(0.1)
    return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", required=True)
    parser.add_argument(
        "--server",
        default=os.environ.get("RCHAT_SERVER", "ws://127.0.0.1:8443"),
    )
    parser.add_argument("--username", required=True)
    parser.add_argument("--password")
    parser.add_argument("--display-name")
    parser.add_argument("--register", action="store_true")
    parser.add_argument("--tls-fingerprint")
    parser.add_argument("--tls-insecure", action="store_true")
    parser.add_argument(
        "--no-e2e",
        action="store_true",
        help="dev-only plaintext chat mode (requires server flag)",
    )
    args = parser.parse_args()

    try:
        with RChatClient(
            data_dir=args.data_dir,
            server=args.server,
            e2e=not args.no_e2e,
            tls_fingerprint=args.tls_fingerprint,
            tls_insecure=args.tls_insecure,
        ) as client:
            session = _saved_session(client)
            if session is not None:
                if session.username != args.username:
                    print(
                        "error: profile is already authenticated as "
                        f"{session.username}",
                        file=sys.stderr,
                    )
                    return 1
                print(f"using the saved session of {session.username}")
            else:
                password = _password(args)
                if args.register:
                    session = client.register(
                        args.username, password, args.display_name
                    )
                else:
                    session = client.login(args.username, password)
                print(
                    f"logged in as "
                    f"{session.username if session else args.username}"
                )
            print("listening for message_received; press Ctrl+C to stop")

            own_user_id = session.user_id if session else None
            for event in client.events():
                if event.event != "message_received":
                    continue
                message = event.data
                if message.get("content_kind") != "text":
                    continue
                if own_user_id and message.get("sender_user_id") == own_user_id:
                    continue
                body = message.get("body")
                if not body:
                    continue
                chat_id = message.get("chat_id")
                if not chat_id:
                    continue
                try:
                    local_id = client.send_message(chat_id, body)
                    print(f"echo -> {chat_id}: {local_id}")
                except RChatException as error:
                    print(f"echo failed: {error}", file=sys.stderr)
    except KeyboardInterrupt:
        print("\nstopped")
        return 0
    except RChatException as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

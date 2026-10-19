#!/usr/bin/env python3
"""Minimal low-level FFI smoke test, analogous to ffi_smoke.c."""

from __future__ import annotations

import argparse
import json
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from rchat import ffi  # noqa: E402
from rchat.exceptions import RChatLibraryError  # noqa: E402


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--data-dir",
        help="temporary profile directory (default: a fresh temp dir)",
    )
    args = parser.parse_args()

    try:
        print(f"library: {ffi.library_path()}")
        print(f"version: {ffi.rchat_version()}")
        print(f"api_version: {ffi.rchat_api_version()}")
    except RChatLibraryError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    with tempfile.TemporaryDirectory(prefix="rchat-ffi-smoke-") as temporary:
        data_dir = args.data_dir or temporary
        handle = ffi.rchat_create(
            {
                "data_dir": data_dir,
                "server": "ws://127.0.0.1:8443",
                "auto_start": False,
            }
        )
        if handle is None:
            print(f"rchat_create: {ffi.rchat_last_error()}", file=sys.stderr)
            return 1
        try:
            response = ffi.rchat_send_json(handle, {"@type": "ping"})
            print(json.dumps(response, ensure_ascii=False, indent=2))
            if response.get("@type") != "ok":
                return 1
        finally:
            # The wrapper has already freed the send/receive strings.
            ffi.rchat_destroy(handle)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

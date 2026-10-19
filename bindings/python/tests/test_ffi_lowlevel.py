"""Low-level ctypes tests for the rchat C ABI.

Run from the repository root after building the library:

    cargo build -p rchat-ffi
    python3 -m unittest discover -s bindings/python/tests
"""

from __future__ import annotations

import ctypes
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from rchat import ffi  # noqa: E402
from rchat.exceptions import (  # noqa: E402
    AuthenticationError,
    InvalidHandleError,
    RChatError,
    UnknownMethodError,
)


class LowLevelFfiTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        # Fail with a useful message instead of an opaque ImportError/OSError.
        try:
            cls.library = ffi.get_library()
        except Exception as error:  # noqa: BLE001 - test bootstrap
            raise RuntimeError(
                "rchat FFI library is not available; run "
                "'cargo build -p rchat-ffi' first"
            ) from error

    def _create(self, directory: str, auto_start: bool = False) -> int:
        handle = ffi.rchat_create(
            {
                "data_dir": directory,
                "server": "ws://127.0.0.1:1",
                "auto_start": auto_start,
            }
        )
        self.assertIsNotNone(handle)
        return int(handle)  # type: ignore[arg-type]

    def test_version_and_api_version(self) -> None:
        self.assertIsInstance(ffi.rchat_version(), str)
        self.assertTrue(ffi.rchat_version())
        self.assertEqual(ffi.rchat_api_version(), 1)

    def test_ctypes_signatures(self) -> None:
        lib = self.library
        self.assertEqual(lib.rchat_create.argtypes, [ctypes.c_char_p])
        self.assertEqual(lib.rchat_create.restype, ctypes.c_void_p)
        self.assertEqual(
            lib.rchat_send_json.argtypes,
            [ctypes.c_void_p, ctypes.c_char_p],
        )
        self.assertEqual(
            lib.rchat_send_json.restype,
            ctypes.POINTER(ctypes.c_char),
        )
        self.assertEqual(
            lib.rchat_receive_json.argtypes,
            [ctypes.c_void_p, ctypes.c_uint64],
        )
        self.assertEqual(lib.rchat_receive_json.restype, ctypes.POINTER(ctypes.c_char))
        self.assertEqual(
            lib.rchat_free_string.argtypes,
            [ctypes.POINTER(ctypes.c_char)],
        )
        self.assertIsNone(lib.rchat_destroy.restype)
        self.assertIsNone(lib.rchat_free_string.restype)
        self.assertEqual(lib.rchat_version.restype, ctypes.c_char_p)
        self.assertEqual(lib.rchat_api_version.restype, ctypes.c_uint32)
        self.assertEqual(lib.rchat_last_error.restype, ctypes.c_char_p)

    def test_create_ping_and_destroy(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            handle = self._create(directory)
            try:
                response = ffi.rchat_send_json(handle, {"@type": "ping"})
                self.assertEqual(response["@type"], "ok")
                self.assertEqual(response["data"]["service"], "rchat-ffi")
                self.assertEqual(response["data"]["api_version"], 1)
                self.assertIsNone(ffi.rchat_receive_json(handle, 0))
            finally:
                ffi.rchat_destroy(handle)

    def test_configuration_errors_set_last_error(self) -> None:
        self.assertIsNone(ffi.rchat_create(b"not json"))
        self.assertTrue(ffi.rchat_last_error())

        with tempfile.TemporaryDirectory() as directory:
            self.assertIsNone(
                ffi.rchat_create(
                    {
                        "data_dir": directory,
                        "server": "ws://127.0.0.1:1",
                        "unknown_field": True,
                    }
                )
            )
            self.assertTrue(ffi.rchat_last_error())

    def test_error_response_factory(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            handle = self._create(directory)
            try:
                response = ffi.rchat_send_json(
                    handle, {"@type": "definitely_not_a_method"}
                )
                self.assertEqual(response["@type"], "error")
                error = RChatError.from_response(response)
                self.assertIsInstance(error, UnknownMethodError)
                self.assertEqual(error.code, "unknown_method")
                self.assertFalse(error.retryable)
            finally:
                ffi.rchat_destroy(handle)

        response = ffi.rchat_send_json(None, {"@type": "ping"})
        self.assertEqual(response["@type"], "error")
        self.assertIsInstance(
            RChatError.from_response(response), InvalidHandleError
        )

    def test_server_error_mapping(self) -> None:
        error = RChatError.from_response(
            {
                "@type": "error",
                "request": "login",
                "code": "server",
                "message": "invalid credentials",
                "retryable": False,
                "server_code": 4,
                "server_code_name": "invalid_credentials",
            }
        )
        self.assertIsInstance(error, AuthenticationError)
        self.assertEqual(error.code, "server")
        self.assertEqual(error.server_code, 4)
        self.assertEqual(error.server_code_name, "invalid_credentials")
        self.assertEqual(error.request, "login")

    def test_returned_strings_are_freed_but_borrowed_strings_are_not(self) -> None:
        original_free = self.library.rchat_free_string
        freed = []

        def counting_free(pointer: object) -> None:
            freed.append(pointer)
            original_free(pointer)

        with tempfile.TemporaryDirectory() as directory:
            handle = self._create(directory)
            self.library.rchat_free_string = counting_free  # type: ignore[assignment]
            try:
                ffi.rchat_send_json(handle, {"@type": "ping"})
                self.assertEqual(len(freed), 1)
                ffi.rchat_receive_json(handle, 0)
                self.assertEqual(len(freed), 1)

                # Borrowed strings must not go through rchat_free_string.
                ffi.rchat_version()
                ffi.rchat_last_error()
                self.assertEqual(len(freed), 1)
            finally:
                self.library.rchat_free_string = original_free  # type: ignore[assignment]
                ffi.rchat_destroy(handle)


if __name__ == "__main__":
    unittest.main()

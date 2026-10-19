"""Low-level ``ctypes`` binding for the rchat C ABI.

The native library is loaded lazily on the first call.  Set
``RCHAT_LIB_PATH`` to a library file or a directory containing it; otherwise
the binding searches ``target/release`` and then ``target/debug`` in the
repository checkout.

Every string returned by ``rchat_send_json``/``rchat_receive_json`` is copied
and freed with ``rchat_free_string`` in a ``finally`` block.  The borrowed
strings returned by ``rchat_version``/``rchat_last_error`` are deliberately
never freed.
"""

from __future__ import annotations

import ctypes
import json
import os
import sys
import threading
from pathlib import Path
from typing import Any, Dict, Mapping, Optional, Tuple, Union

from .exceptions import RChatError, RChatLibraryError

JsonInput = Union[Mapping[str, Any], str, bytes]

_c_char_p = ctypes.c_char_p
_c_void_p = ctypes.c_void_p
_c_uint64 = ctypes.c_uint64
_c_uint32 = ctypes.c_uint32
_c_char_pointer = ctypes.POINTER(ctypes.c_char)

_LIBRARY: Optional[ctypes.CDLL] = None
_LIBRARY_LOCK = threading.Lock()


def _repo_root() -> Path:
    # <repo>/bindings/python/rchat/ffi.py
    return Path(__file__).resolve().parents[3]


def _library_names() -> Tuple[str, ...]:
    if sys.platform == "win32":
        return ("rchat_ffi.dll",)
    if sys.platform == "darwin":
        return ("librchat_ffi.dylib",)
    return ("librchat_ffi.so",)


def _find_in_directory(directory: Path) -> Optional[Path]:
    for name in _library_names():
        candidate = directory / name
        if candidate.is_file():
            return candidate
    return None


def _find_library() -> Path:
    configured = os.environ.get("RCHAT_LIB_PATH", "").strip()
    if configured:
        path = Path(configured).expanduser()
        candidates = [path]
        if not path.is_absolute():
            candidates.append(_repo_root() / path)
        for candidate in candidates:
            if candidate.is_file():
                return candidate
            if candidate.is_dir():
                found = _find_in_directory(candidate)
                if found is not None:
                    return found
        raise RChatLibraryError(
            "RCHAT_LIB_PATH does not point to a rchat FFI library: "
            f"{configured!r}"
        )

    root = _repo_root()
    tried = []
    for profile in ("release", "debug"):
        directory = root / "target" / profile
        tried.append(str(directory))
        found = _find_in_directory(directory)
        if found is not None:
            return found

    names = ", ".join(_library_names())
    raise RChatLibraryError(
        f"cannot find {names}; build it with 'cargo build -p rchat-ffi' "
        f"or set RCHAT_LIB_PATH. Searched: {', '.join(tried)}"
    )


def _load_library() -> ctypes.CDLL:
    global _LIBRARY
    if _LIBRARY is not None:
        return _LIBRARY
    with _LIBRARY_LOCK:
        if _LIBRARY is not None:
            return _LIBRARY
        path = _find_library()
        try:
            library = ctypes.CDLL(str(path))
            library.rchat_create.argtypes = [_c_char_p]
            library.rchat_create.restype = _c_void_p
            library.rchat_destroy.argtypes = [_c_void_p]
            library.rchat_destroy.restype = None
            library.rchat_send_json.argtypes = [_c_void_p, _c_char_p]
            library.rchat_send_json.restype = _c_char_pointer
            library.rchat_receive_json.argtypes = [_c_void_p, _c_uint64]
            library.rchat_receive_json.restype = _c_char_pointer
            library.rchat_free_string.argtypes = [_c_char_pointer]
            library.rchat_free_string.restype = None
            library.rchat_version.argtypes = []
            library.rchat_version.restype = _c_char_p
            library.rchat_api_version.argtypes = []
            library.rchat_api_version.restype = _c_uint32
            library.rchat_last_error.argtypes = []
            library.rchat_last_error.restype = _c_char_p
        except (AttributeError, OSError) as error:
            raise RChatLibraryError(
                f"cannot load rchat FFI library {path}: {error}"
            ) from error
        _LIBRARY = library
        return library


def get_library() -> ctypes.CDLL:
    """Return the loaded CDLL (loading it first if necessary)."""
    return _load_library()


def reset_library() -> None:
    """Forget the cached CDLL.  Intended for tests and reconfiguration."""
    global _LIBRARY
    with _LIBRARY_LOCK:
        _LIBRARY = None


load_library = get_library


def library_path() -> Path:
    """Return the library path that would be loaded."""
    return _find_library()


def _encode(payload: Any) -> bytes:
    if isinstance(payload, bytes):
        return payload
    if isinstance(payload, str):
        return payload.encode("utf-8")
    return json.dumps(
        payload,
        ensure_ascii=False,
        separators=(",", ":"),
        allow_nan=False,
    ).encode("utf-8")


def _borrowed_text(value: Optional[bytes]) -> Optional[str]:
    if value is None:
        return None
    return value.decode("utf-8", "replace")


def _last_error(library: ctypes.CDLL) -> Optional[str]:
    return _borrowed_text(library.rchat_last_error())


def _consume_string(library: ctypes.CDLL, pointer: Any) -> str:
    """Copy a returned C string and always release it."""
    try:
        value = ctypes.cast(pointer, _c_char_p).value
        if value is None:
            raise RChatError("internal", "rchat returned a NULL string")
        return value.decode("utf-8")
    finally:
        library.rchat_free_string(pointer)


def _parse_returned(text: str, operation: str) -> Dict[str, Any]:
    try:
        value = json.loads(text)
    except (TypeError, ValueError) as error:
        raise RChatError(
            "internal", f"{operation} returned invalid JSON: {error}"
        ) from error
    if not isinstance(value, dict):
        raise RChatError(
            "internal", f"{operation} returned a non-object JSON value"
        )
    return value


def rchat_create(config: JsonInput) -> Optional[int]:
    """Create a native handle.

    Returns ``None`` on a configuration/initialization error.  Call
    :func:`rchat_last_error` for the diagnostic text.
    """
    library = _load_library()
    return library.rchat_create(_encode(config)) or None


def rchat_destroy(handle: Optional[int]) -> None:
    """Destroy a handle.  ``None`` (NULL) is harmless."""
    library = _load_library()
    library.rchat_destroy(handle)


def rchat_send_json(
    handle: Optional[int], payload: JsonInput
) -> Dict[str, Any]:
    """Send one JSON request and return the parsed JSON response."""
    library = _load_library()
    pointer = library.rchat_send_json(handle, _encode(payload))
    if not pointer:
        message = _last_error(library) or "rchat_send_json returned NULL"
        raise RChatError("internal", message)
    text = _consume_string(library, pointer)
    return _parse_returned(text, "rchat_send_json")


def rchat_receive_json(
    handle: Optional[int], timeout_ms: int = 1000
) -> Optional[Dict[str, Any]]:
    """Receive one JSON event, or ``None`` on timeout/empty queue."""
    if not isinstance(timeout_ms, int) or timeout_ms < 0:
        raise ValueError("timeout_ms must be a non-negative integer")
    library = _load_library()
    pointer = library.rchat_receive_json(handle, timeout_ms)
    if not pointer:
        return None
    text = _consume_string(library, pointer)
    return _parse_returned(text, "rchat_receive_json")


def rchat_free_string(pointer: Any) -> None:
    """Free a string returned by the send/receive functions."""
    library = _load_library()
    library.rchat_free_string(pointer)


def rchat_version() -> str:
    """Return the native library version (borrowed C string, not freed)."""
    library = _load_library()
    return _borrowed_text(library.rchat_version()) or ""


def rchat_api_version() -> int:
    """Return the JSON API version implemented by the native library."""
    library = _load_library()
    return int(library.rchat_api_version())


def rchat_last_error() -> Optional[str]:
    """Return the current thread's last ``rchat_create`` error, if any."""
    library = _load_library()
    return _last_error(library)


# Short aliases used by the high-level client and by the examples.
create = rchat_create
destroy = rchat_destroy
send_json = rchat_send_json
receive_json = rchat_receive_json
free_string = rchat_free_string
version = rchat_version
api_version = rchat_api_version
last_error = rchat_last_error

__all__ = [
    "create",
    "destroy",
    "send_json",
    "receive_json",
    "free_string",
    "version",
    "api_version",
    "last_error",
    "get_library",
    "load_library",
    "library_path",
    "reset_library",
    "rchat_create",
    "rchat_destroy",
    "rchat_send_json",
    "rchat_receive_json",
    "rchat_free_string",
    "rchat_version",
    "rchat_api_version",
    "rchat_last_error",
]

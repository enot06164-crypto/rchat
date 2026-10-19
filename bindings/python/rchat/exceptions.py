"""Exception hierarchy for the rchat FFI JSON interface.

The Rust library reports failures as JSON objects with ``@type == "error"``.
:meth:`RChatError.from_response` turns such an object into the most specific
Python exception while keeping the stable FFI ``code`` and the optional server
error details.
"""

from __future__ import annotations

from collections.abc import Mapping as MappingABC
from typing import Any, Mapping, Optional, Type


class RChatException(Exception):
    """Base class for every error raised by the Python SDK."""


class RChatLibraryError(RChatException):
    """The native ``librchat_ffi`` could not be found or loaded."""


class RChatError(RChatException):
    """An error returned by the native FFI as a JSON error response."""

    default_code = "internal"

    def __init__(
        self,
        code: str,
        message: str,
        retryable: bool = False,
        server_code: Optional[int] = None,
        server_code_name: Optional[str] = None,
        request: Optional[str] = None,
    ) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.retryable = bool(retryable)
        self.server_code = server_code
        self.server_code_name = server_code_name
        self.request = request

    def __str__(self) -> str:
        text = f"{self.code}: {self.message}"
        if self.server_code_name:
            text += f" (server_code={self.server_code_name}"
            if self.server_code is not None:
                text += f"/{self.server_code}"
            text += ")"
        if self.retryable:
            text += " [retryable]"
        return text

    @classmethod
    def from_response(cls, response: Mapping[str, Any]) -> "RChatError":
        """Build the appropriate exception from a JSON error response.

        The returned object is not raised automatically; callers normally do
        ``raise RChatError.from_response(response)``.
        """
        if not isinstance(response, MappingABC) or response.get("@type") != "error":
            raise ValueError("response is not a JSON error object")

        code = str(response.get("code") or cls.default_code)
        message = str(response.get("message") or "rchat operation failed")
        retryable = bool(response.get("retryable", False))
        request = response.get("request")
        server_code = response.get("server_code")
        server_code_name = response.get("server_code_name")
        if not isinstance(server_code, int):
            server_code = None
        if not isinstance(server_code_name, str):
            server_code_name = None

        error_type: Type[RChatError] = _ERROR_TYPES.get(code, RChatError)
        if code == "server" and server_code_name:
            error_type = _SERVER_ERROR_TYPES.get(server_code_name, ServerError)

        return error_type(
            code,
            message,
            retryable=retryable,
            server_code=server_code,
            server_code_name=server_code_name,
            request=request if isinstance(request, str) else None,
        )


class InvalidRequestError(RChatError):
    """Malformed JSON, invalid fields, or an unsupported local request."""


class InvalidHandleError(InvalidRequestError):
    """A NULL or destroyed handle was passed to the native layer."""


class UnknownMethodError(InvalidRequestError):
    """The JSON request contains an unknown ``@type``."""


class RChatConfigError(InvalidRequestError):
    """The native client rejected the configuration passed to ``create``."""

    def __init__(self, message: str) -> None:
        super().__init__("invalid_request", message)


class AuthenticationError(RChatError):
    """The account is not authenticated or the session is no longer valid."""


class TransportError(RChatError):
    """A network or connection error occurred."""


class NotConnectedError(TransportError):
    """The client is currently disconnected; the operation may be retried."""


class TimeoutError(TransportError):
    """The operation or a connection attempt timed out."""


class StorageError(RChatError):
    """A local storage error occurred."""


class SecretStoreError(RChatError):
    """The encrypted local secret store could not be read or written."""


class CryptoError(RChatError):
    """The E2E provider reported an error."""


class ProtocolError(RChatError):
    """The peer or server violated the protocol."""


class ResyncRequiredError(ProtocolError):
    """The server requested a full resynchronization."""


class NotFoundError(RChatError):
    """The requested object does not exist."""


class E2eUnavailableError(RChatError):
    """E2E is not configured or suitable keys are unavailable."""


class ClosedError(RChatError):
    """The native client was shut down."""


class RChatClosedError(ClosedError):
    """The Python client handle is closed locally."""

    def __init__(self, message: str = "rchat client is closed") -> None:
        super().__init__("closed", message)


class PanicError(RChatError):
    """A Rust panic was caught at the C boundary."""


class InternalError(RChatError):
    """An unexpected internal error occurred."""


class ServerError(RChatError):
    """The server returned an error without a more specific mapping."""


class ConflictError(ServerError):
    """The server rejected the operation because of a conflict."""


class UsernameTakenError(ConflictError):
    """Registration requested a username that is already in use."""


class ForbiddenError(ServerError):
    """The server denied the operation."""


class RateLimitedError(ServerError):
    """The server rate limit was exceeded."""


class UnsupportedCapabilityError(ServerError):
    """The server or peer does not support a requested capability."""


_ERROR_TYPES: Mapping[str, Type[RChatError]] = {
    "invalid_request": InvalidRequestError,
    "invalid_handle": InvalidHandleError,
    "unknown_method": UnknownMethodError,
    "server": ServerError,
    "transport": TransportError,
    "storage": StorageError,
    "secret_store": SecretStoreError,
    "crypto": CryptoError,
    "not_authenticated": AuthenticationError,
    "protocol": ProtocolError,
    "not_found": NotFoundError,
    "e2e_unavailable": E2eUnavailableError,
    "not_connected": NotConnectedError,
    "timeout": TimeoutError,
    "closed": ClosedError,
    "panic": PanicError,
    "internal": InternalError,
}

_SERVER_ERROR_TYPES: Mapping[str, Type[RChatError]] = {
    "invalid_request": InvalidRequestError,
    "unsupported_method": UnsupportedCapabilityError,
    "protocol_version_mismatch": ProtocolError,
    "invalid_credentials": AuthenticationError,
    "username_taken": UsernameTakenError,
    "username_invalid": InvalidRequestError,
    "unauthenticated": AuthenticationError,
    "session_expired": AuthenticationError,
    "session_revoked": AuthenticationError,
    "forbidden": ForbiddenError,
    "not_found": NotFoundError,
    "conflict": ConflictError,
    "device_conflict": ConflictError,
    "rate_limited": RateLimitedError,
    "payload_too_large": InvalidRequestError,
    "e2e_required": E2eUnavailableError,
    "e2e_keys_missing": E2eUnavailableError,
    "unsupported_capability": UnsupportedCapabilityError,
    "resync_required": ResyncRequiredError,
    "limit_exceeded": InvalidRequestError,
    "internal": ServerError,
}

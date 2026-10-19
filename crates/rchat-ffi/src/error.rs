//! Ошибки JSON-интерфейса и их стабильное представление для C-хоста.

use rchat_core::error::CoreError;
use rchat_proto::proto::ErrorCode;
use serde_json::{json, Value};

/// Ошибка обработки запроса. Не содержит секретов: сообщения ядра и сервера
/// уже сформулированы для диагностики, а токены и ключи в них не попадают.
#[derive(Debug, Clone)]
pub(crate) struct FfiError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
    pub(crate) retryable: bool,
    pub(crate) server_code: Option<i32>,
    pub(crate) server_code_name: Option<&'static str>,
}

impl FfiError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            server_code: None,
            server_code_name: None,
        }
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_request", message)
    }

    pub(crate) fn unknown_method(method: &str) -> Self {
        Self::new(
            "unknown_method",
            format!("unknown request @type {method:?}"),
        )
    }

    pub(crate) fn invalid_handle() -> Self {
        Self::new("invalid_handle", "handle is null or was destroyed")
    }

    /// JSON-ответ с ошибкой. `request` добавляется, если метод уже разобран.
    pub(crate) fn to_response(&self, request: Option<&str>) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("@type".to_string(), json!("error"));
        if let Some(request) = request {
            object.insert("request".to_string(), json!(request));
        }
        object.insert("code".to_string(), json!(self.code));
        object.insert("message".to_string(), json!(self.message));
        object.insert("retryable".to_string(), json!(self.retryable));
        if let Some(code) = self.server_code {
            object.insert("server_code".to_string(), json!(code));
        }
        if let Some(name) = self.server_code_name {
            object.insert("server_code_name".to_string(), json!(name));
        }
        Value::Object(object)
    }
}

impl From<CoreError> for FfiError {
    fn from(error: CoreError) -> Self {
        match error {
            CoreError::Rejected {
                code,
                raw_code,
                message,
                retryable,
            } => Self {
                code: "server",
                message,
                retryable,
                server_code: Some(raw_code),
                server_code_name: code.map(server_code_name),
            },
            CoreError::Transport(error) => Self {
                code: "transport",
                message: error.to_string(),
                retryable: true,
                server_code: None,
                server_code_name: None,
            },
            CoreError::Storage(error) => Self::new("storage", error.to_string()),
            CoreError::Secret(error) => Self::new("secret_store", error.to_string()),
            CoreError::Crypto(error) => Self::new("crypto", error.to_string()),
            CoreError::NotAuthenticated => Self::new("not_authenticated", "not authenticated"),
            CoreError::Protocol(message) => Self::new("protocol", message),
            CoreError::InvalidInput(message) => Self::invalid(message),
            CoreError::NotFound(message) => Self::new("not_found", message),
            CoreError::E2eUnavailable(message) => Self::new("e2e_unavailable", message),
            CoreError::NotConnected => Self {
                code: "not_connected",
                message: "not connected".to_string(),
                retryable: true,
                server_code: None,
                server_code_name: None,
            },
            CoreError::Timeout => Self {
                code: "timeout",
                message: "operation timed out".to_string(),
                retryable: true,
                server_code: None,
                server_code_name: None,
            },
            CoreError::Closed => Self::new("closed", "client is shut down"),
            CoreError::Internal(message) => Self::new("internal", message),
        }
    }
}

impl From<rchat_core::error::StorageError> for FfiError {
    fn from(error: rchat_core::error::StorageError) -> Self {
        Self::from(CoreError::Storage(error))
    }
}

/// Стабильные имена серверных кодов. Совпадают с enum `ErrorCode` в
/// `common.proto` в нижнем регистре; незнакомый код остаётся числом.
fn server_code_name(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Unspecified => "unspecified",
        ErrorCode::InvalidRequest => "invalid_request",
        ErrorCode::UnsupportedMethod => "unsupported_method",
        ErrorCode::ProtocolVersionMismatch => "protocol_version_mismatch",
        ErrorCode::InvalidCredentials => "invalid_credentials",
        ErrorCode::UsernameTaken => "username_taken",
        ErrorCode::UsernameInvalid => "username_invalid",
        ErrorCode::Unauthenticated => "unauthenticated",
        ErrorCode::SessionExpired => "session_expired",
        ErrorCode::SessionRevoked => "session_revoked",
        ErrorCode::Forbidden => "forbidden",
        ErrorCode::NotFound => "not_found",
        ErrorCode::Conflict => "conflict",
        ErrorCode::DeviceConflict => "device_conflict",
        ErrorCode::RateLimited => "rate_limited",
        ErrorCode::PayloadTooLarge => "payload_too_large",
        ErrorCode::E2eRequired => "e2e_required",
        ErrorCode::E2eKeysMissing => "e2e_keys_missing",
        ErrorCode::UnsupportedCapability => "unsupported_capability",
        ErrorCode::Internal => "internal",
        ErrorCode::ResyncRequired => "resync_required",
        ErrorCode::LimitExceeded => "limit_exceeded",
    }
}

//! Ошибки серверной стороны.
//!
//! Ошибка содержит протокольный код, безопасное для клиента сообщение и
//! признак «можно повторить». Внутренние детали (SQL, стек) никогда не
//! попадают в `message` — они логируются отдельно.

use rchat_proto::proto::ErrorCode;

use crate::repo::RepoError;

/// Ошибка обработки запроса, готовая к отображению в протокольный `Error`.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct ServerError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

impl ServerError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
        }
    }

    pub fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidRequest, message)
    }

    pub fn unsupported_method() -> Self {
        Self::new(ErrorCode::UnsupportedMethod, "unsupported method")
    }

    pub fn protocol_version_mismatch(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ProtocolVersionMismatch, message)
    }

    pub fn invalid_credentials() -> Self {
        // Политика: не различаем «нет такого пользователя» и «неверный пароль».
        Self::new(
            ErrorCode::InvalidCredentials,
            "invalid username or password",
        )
    }

    pub fn username_taken() -> Self {
        Self::new(ErrorCode::UsernameTaken, "username is already taken")
    }

    pub fn username_invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::UsernameInvalid, message)
    }

    pub fn unauthenticated() -> Self {
        Self::new(ErrorCode::Unauthenticated, "authentication required")
    }

    pub fn session_expired() -> Self {
        Self::new(ErrorCode::SessionExpired, "session has expired")
    }

    pub fn session_revoked() -> Self {
        Self::new(ErrorCode::SessionRevoked, "session has been revoked")
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Forbidden, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Conflict, message)
    }

    pub fn device_conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::DeviceConflict, message)
    }

    pub fn rate_limited() -> Self {
        Self::new(ErrorCode::RateLimited, "too many requests; try again later").retryable()
    }

    pub fn payload_too_large(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::PayloadTooLarge, message)
    }

    pub fn e2e_required(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::E2eRequired, message)
    }

    pub fn e2e_keys_missing(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::E2eKeysMissing, message)
    }

    pub fn unsupported_capability(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::UnsupportedCapability, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message).retryable()
    }

    pub fn resync_required() -> Self {
        Self::new(ErrorCode::ResyncRequired, "sync cursor is out of retention")
    }

    pub fn limit_exceeded(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::LimitExceeded, message)
    }
}

impl From<RepoError> for ServerError {
    fn from(error: RepoError) -> Self {
        match error {
            RepoError::Conflict(key) => match key.as_str() {
                "username_taken" => ServerError::username_taken(),
                "chat_pair" => ServerError::conflict("direct chat already exists"),
                "group_full" => ServerError::limit_exceeded("group chat member limit reached"),
                other => ServerError::conflict(other.to_string()),
            },
            RepoError::NotFound(detail) => {
                tracing::debug!(detail = %detail, "repo: not found");
                ServerError::not_found("resource not found")
            }
            RepoError::Internal(detail) => {
                // Детали — только в лог, наружу безопасное сообщение.
                tracing::error!(detail = %detail, "repo: internal error");
                ServerError::internal("internal storage error")
            }
        }
    }
}

//! Типизированные ошибки ядра и его адаптеров.

use rchat_proto::proto::ErrorCode;

/// Ошибка транспорта. Не содержит секретов; детали — для диагностики.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{kind}: {message}")]
pub struct TransportError {
    pub kind: TransportErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportErrorKind {
    /// Не удалось установить соединение (I/O, TLS, DNS).
    Connect,
    /// Соединение потеряно или закрыто неожиданно.
    Closed,
    /// Ошибка на уровне кадра/протокола WebSocket.
    Frame,
    /// Тайм-аут (в том числе heartbeat).
    Timeout,
    /// Прочее.
    Other,
}

impl std::fmt::Display for TransportErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Connect => "connect",
            Self::Closed => "closed",
            Self::Frame => "frame",
            Self::Timeout => "timeout",
            Self::Other => "other",
        };
        f.write_str(name)
    }
}

impl TransportError {
    pub fn new(kind: TransportErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn connect(message: impl Into<String>) -> Self {
        Self::new(TransportErrorKind::Connect, message)
    }

    pub fn closed(message: impl Into<String>) -> Self {
        Self::new(TransportErrorKind::Closed, message)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("storage transaction failed: {0}")]
    Transaction(String),
    #[error("storage constraint violation: {0}")]
    Constraint(String),
    #[error("storage not found: {0}")]
    NotFound(String),
    #[error("storage I/O: {0}")]
    Io(String),
    #[error("storage internal: {0}")]
    Internal(String),
}

#[derive(Debug, thiserror::Error)]
pub enum SecretStoreError {
    #[error("secret store unavailable: {0}")]
    Unavailable(String),
    #[error("secret not found: {0}")]
    NotFound(String),
    #[error("secret store corrupted: {0}")]
    Corrupted(String),
    #[error("secret store I/O: {0}")]
    Io(String),
}

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("crypto state unavailable: {0}")]
    State(String),
    #[error("no prekey bundle for device {0}")]
    MissingPreKey(String),
    #[error("decryption failed for device {0}: {1}")]
    DecryptFailed(String, String),
    #[error("session establishment failed for device {0}: {1}")]
    Session(String, String),
    #[error("peer identity key mismatch for device {0}")]
    IdentityMismatch(String),
    #[error("crypto internal error: {0}")]
    Internal(String),
}

/// Ошибка публичного API ядра.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("secret store error: {0}")]
    Secret(#[from] SecretStoreError),
    #[error("crypto error: {0}")]
    Crypto(#[from] CryptoError),
    /// Операция требует аутентификации, но сессия отсутствует/недействительна.
    #[error("not authenticated")]
    NotAuthenticated,
    /// Сервер вернул типизированную ошибку.
    #[error("server rejected request ({raw_code}): {message}")]
    Rejected {
        code: Option<ErrorCode>,
        raw_code: i32,
        message: String,
        retryable: bool,
    },
    /// Нарушение протокола/неожиданный ответ.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// Некорректные входные данные от вызывающего кода.
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("not found: {0}")]
    NotFound(String),
    /// E2E требуется, но недоступен (нет провайдера, ключей, сессии).
    #[error("e2e unavailable: {0}")]
    E2eUnavailable(String),
    /// Соединение не было установлено в отведённое время.
    #[error("not connected")]
    NotConnected,
    #[error("operation timed out")]
    Timeout,
    /// Клиент остановлен.
    #[error("client is shut down")]
    Closed,
    #[error("internal error: {0}")]
    Internal(String),
}

impl CoreError {
    /// Ошибка сервера помечена как постоянная (не повторять бесконечно).
    pub fn is_permanent(&self) -> bool {
        match self {
            CoreError::Rejected { retryable, .. } => !retryable,
            CoreError::InvalidInput(_) | CoreError::NotFound(_) => true,
            _ => false,
        }
    }

    /// Ошибка авторизации (требуется повторный вход).
    pub fn is_auth_error(&self) -> bool {
        matches!(
            self,
            CoreError::NotAuthenticated
                | CoreError::Rejected {
                    code: Some(
                        ErrorCode::Unauthenticated
                            | ErrorCode::SessionExpired
                            | ErrorCode::SessionRevoked
                            | ErrorCode::InvalidCredentials
                    ),
                    ..
                }
        )
    }
}

/// Создать ошибку `Rejected` из типизированной ошибки протокола.
pub fn rejected_from(error: &rchat_proto::Error) -> CoreError {
    CoreError::Rejected {
        code: rchat_proto::error_code(error),
        raw_code: error.code,
        message: error.message.clone(),
        retryable: error.retryable,
    }
}

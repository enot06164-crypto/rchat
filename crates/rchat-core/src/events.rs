//! Типизированные события для UI (CLI и будущих клиентов).
//!
//! События — уведомления, а не источник истины: состояние всегда можно
//! перечитать из `Storage`/сервера. Подписчик может потерять события при
//! переполнении канала (`EventError::Lagged`).

use tokio::sync::broadcast;

use crate::types::{
    ChatRecord, ConnectionState, MessageRecord, OutboxState, ReceiptRecord, SyncProgress,
};

#[derive(Debug, Clone)]
pub enum UiEvent {
    ConnectionStateChanged(ConnectionState),
    /// Сессия активна (после login/resume).
    SessionStarted {
        user_id: String,
        username: String,
    },
    /// Сессия завершена; `reason` — для диагностики.
    SessionEnded {
        reason: String,
    },
    /// Новое сообщение (включая собственные, доставленные на другие устройства).
    MessageReceived(MessageRecord),
    /// Сообщение изменено (edit/delete).
    MessageUpdated(MessageRecord),
    /// Изменилось состояние отправки собственного сообщения.
    OutboxChanged {
        local_id: String,
        state: OutboxState,
        error: Option<String>,
    },
    ChatUpserted(ChatRecord),
    ReceiptUpdated(ReceiptRecord),
    PresenceChanged {
        user_id: String,
        online: bool,
    },
    SyncProgress(SyncProgress),
    /// Выполнен snapshot после `ResyncRequired`.
    ResyncPerformed {
        up_to_seq: u64,
    },
    /// Не удалось расшифровать E2E-сообщение (устройство/сессия недоступны).
    E2eDecryptFailed {
        chat_id: String,
        server_id: u64,
        device_id: String,
    },
    /// Некритичное предупреждение (например, отозванное устройство в чате).
    Warning(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EventError {
    /// Клиент остановлен, событий больше не будет.
    #[error("event stream closed")]
    Closed,
    /// Подписчик не успевал; `n` событий потеряно. Состояние хранилища актуально.
    #[error("event stream lagged, {0} events lost")]
    Lagged(u64),
}

/// Подписка на события. Тип скрывает детали runtime.
pub struct EventSubscription {
    rx: broadcast::Receiver<UiEvent>,
}

impl EventSubscription {
    pub(crate) fn new(rx: broadcast::Receiver<UiEvent>) -> Self {
        Self { rx }
    }

    pub async fn next(&mut self) -> Result<UiEvent, EventError> {
        match self.rx.recv().await {
            Ok(event) => Ok(event),
            Err(broadcast::error::RecvError::Lagged(n)) => Err(EventError::Lagged(n)),
            Err(broadcast::error::RecvError::Closed) => Err(EventError::Closed),
        }
    }
}

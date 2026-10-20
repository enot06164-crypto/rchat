//! Типизированные события для UI (CLI и будущих клиентов).
//!
//! События — уведомления, а не источник истины: состояние всегда можно
//! перечитать из `Storage`/сервера. Подписчик может потерять события при
//! переполнении канала (`EventError::Lagged`).

use tokio::sync::broadcast;

use crate::types::{
    CallRecord, ChatRecord, ConnectionState, MessageRecord, OutboxState, ReceiptRecord,
    SyncProgress,
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
        /// Код ошибки сервера (если ошибка пришла от сервера): позволяет
        /// отличить «нет доступа» от «превышен лимит» без разбора текста.
        error_code: Option<i32>,
    },
    ChatUpserted(ChatRecord),
    ReceiptUpdated(ReceiptRecord),
    PresenceChanged {
        user_id: String,
        online: bool,
    },
    /// Входящий звонок (ringing; только сигнализация).
    CallInvited(CallRecord),
    /// Звонок принят (ringing → active); событие видят все устройства
    /// участников, включая принявшего.
    CallAccepted(CallRecord),
    /// Звонок отклонён callee.
    CallDeclined(CallRecord),
    /// Звонок завершён (явно, по таймауту или из-за ухода в офлайн).
    CallEnded(CallRecord),
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
    /// Собеседник предлагает сменить режим личного чата: в приватный (E2E)
    /// или обратно в обычный. `target_mode` — к чему просят перевести.
    PrivateModeRequested {
        chat_id: String,
        requester_user_id: String,
        requester_username: String,
        requester_display_name: String,
        target_mode: i32,
    },
    /// Запрос на приватный режим отозван тем, кто его отправил.
    PrivateModeCancelled {
        chat_id: String,
    },
    /// Запрос разрешён: `accepted` — чат переведён в приватный режим,
    /// иначе собеседник его отклонил.
    PrivateModeResolved {
        chat_id: String,
        accepted: bool,
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

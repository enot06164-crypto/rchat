//! Доменные записи клиента. Это типы локального состояния, а не wire-схема:
//! они не содержат секретов в незашифрованном виде и не зависят от конкретных
//! реализаций хранилищ.

use rchat_proto::proto::{ChatKind, ChatMemberRole, E2eMode};

/// Локальный аккаунт (профиль) на этом устройстве.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalAccount {
    pub user_id: String,
    pub username: String,
    pub display_name: String,
    pub device_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRecord {
    pub chat_id: String,
    pub kind: ChatKind,
    pub e2e_mode: E2eMode,
    pub title: String,
    /// Для личного чата — user_id собеседника (с точки зрения этого устройства).
    pub peer_user_id: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub last_server_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRecord {
    pub user_id: String,
    pub username: String,
    pub display_name: String,
    /// Роль в групповом чате; для личных чатов — UNSPECIFIED.
    pub role: ChatMemberRole,
    pub joined_at_ms: u64,
}

/// Пользователь (кеш профилей собеседников и себя).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRecord {
    pub user_id: String,
    pub username: String,
    pub display_name: String,
}

/// Устройство пользователя (своё или собеседника). Хранятся только публичные ключи.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRecord {
    pub device_id: String,
    pub user_id: String,
    pub name: String,
    pub platform: String,
    pub e2e_version: u32,
    pub ed25519: Vec<u8>,
    pub curve25519: Vec<u8>,
    pub revoked_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageStatus {
    Pending,
    Sending,
    Sent,
    Failed,
}

impl MessageStatus {
    pub fn as_i64(self) -> i64 {
        match self {
            MessageStatus::Pending => 0,
            MessageStatus::Sending => 1,
            MessageStatus::Sent => 2,
            MessageStatus::Failed => 3,
        }
    }

    pub fn from_i64(value: i64) -> Option<Self> {
        match value {
            0 => Some(MessageStatus::Pending),
            1 => Some(MessageStatus::Sending),
            2 => Some(MessageStatus::Sent),
            3 => Some(MessageStatus::Failed),
            _ => None,
        }
    }
}

/// Сообщение в локальном хранилище.
///
/// `local_id` — постоянный клиентский идентификатор. Для исходящих сообщений он
/// же является ключом идемпотентности (`client_message_id`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRecord {
    pub local_id: String,
    pub server_id: Option<u64>,
    pub chat_id: String,
    pub sender_user_id: String,
    pub sender_device_id: String,
    pub content_kind: String,
    /// Расшифрованный текст. Локальная копия; на сервер не отправляется.
    pub body: Option<String>,
    pub reply_to_server_id: Option<u64>,
    /// Статус отправки; заполнен только для собственных сообщений.
    pub status: Option<MessageStatus>,
    pub created_at_ms: u64,
    pub edited_at_ms: Option<u64>,
    pub deleted_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxState {
    Pending,
    Sending,
    Sent,
    Failed,
}

impl OutboxState {
    pub fn as_i64(self) -> i64 {
        match self {
            OutboxState::Pending => 0,
            OutboxState::Sending => 1,
            OutboxState::Sent => 2,
            OutboxState::Failed => 3,
        }
    }

    pub fn from_i64(value: i64) -> Option<Self> {
        match value {
            0 => Some(OutboxState::Pending),
            1 => Some(OutboxState::Sending),
            2 => Some(OutboxState::Sent),
            3 => Some(OutboxState::Failed),
            _ => None,
        }
    }
}

/// Запись очереди отправки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxRecord {
    pub local_id: String,
    pub chat_id: String,
    /// Ключ идемпотентности; равен `local_id` для сообщений.
    pub idempotency_key: String,
    /// Сериализованный `SendMessageRequest`.
    pub request: Vec<u8>,
    pub state: OutboxState,
    pub attempts: u32,
    pub next_attempt_at_ms: u64,
    pub last_error_code: Option<i32>,
    pub last_error: Option<String>,
    pub created_at_ms: u64,
    pub server_message_id: Option<u64>,
}

/// Прогресс доставки/прочтения со стороны участника чата.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptRecord {
    pub chat_id: String,
    pub user_id: String,
    pub delivered_up_to: u64,
    pub read_up_to: u64,
    pub updated_at_ms: u64,
}

/// Мутация локального состояния, применяемая атомарно вместе с продвижением
/// курсора синхронизации. Все операции идемпотентны по построению.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncApply {
    UpsertChat {
        chat: ChatRecord,
        members: Vec<MemberRecord>,
    },
    /// Вставка полученного сообщения. Дубликаты по `server_id` игнорируются
    /// хранилищем (повторная доставка события безопасна).
    InsertMessage(MessageRecord),
    EditMessage {
        server_id: u64,
        content_kind: String,
        /// `None` — расшифровать не удалось; сохраняется прежний текст.
        body: Option<String>,
        edited_at_ms: u64,
    },
    DeleteMessage {
        server_id: u64,
        deleted_at_ms: u64,
    },
    /// Обновление прогресса. Хранилище применяет монотонно (max).
    UpsertReceipt(ReceiptRecord),
    UpsertUser(UserRecord),
    UpsertDevice(DeviceRecord),
    /// Устройство отозвано: локальный кеш помечает его, сессия удаляется ядром.
    DeviceRevoked {
        device_id: String,
        revoked_at_ms: u64,
    },
    /// Событие неизвестного типа: пропущено осознанно, но курсор продвигается.
    Skip {
        seq: u64,
        kind_hint: u32,
    },
}

/// Результат синхронизации для пользователя CLI/UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyncProgress {
    pub from_seq: u64,
    pub to_seq: u64,
    pub events_applied: u64,
    pub events_skipped: u64,
    pub resync_performed: bool,
}

/// Состояние соединения, публикуемое подписчикам.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    /// Соединение и handshake прошли, но сессия не активна (нужен вход).
    Unauthenticated,
    /// Активная сессия, обмен идёт.
    Connected,
    /// Реконнект после сбоя.
    Reconnecting,
}

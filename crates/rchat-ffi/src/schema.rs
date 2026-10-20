//! Формальная JSON Schema протокола `rchat-ffi`.
//!
//! Типы модуля описывают контракт из `docs/ffi.md`: входящие команды,
//! успешные/ошибочные ответы, сущности и события очереди. Они не участвуют в
//! рантайм-диспетчеризации (там используется `serde_json::Value`), но служат
//! единым источником истины для генерации типизированных клиентов.
//!
//! Файл `schema/rchat.schema.json` создаётся утилитой
//! `cargo run -p rchat-ffi --bin export_schema`; тест
//! `test_schema_is_up_to_date` не даёт схеме устареть.

// Типы модуля описывают только JSON Schema и не создаются в рантайме,
// поэтому большой размер вариантов объединений здесь не важен.
#![allow(clippy::large_enum_variant)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Корневая схема: любое сообщение протокола.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(
    title = "rchat-ffi JSON protocol",
    description = "Запросы, ответы и события C-ABI/JSON rchat (стиль tdjson), API version 1."
)]
pub enum RchatMessage {
    Command(Command),
    Ok(OkResponse),
    Error(ErrorResponse),
    Event(Event),
}

/// Входящая команда. Поле `@type` — дискриминатор.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "@type", rename_all = "snake_case")]
pub enum Command {
    /// Проверка связи без сети.
    Ping {},
    /// Запустить фоновый актор.
    Start {},
    /// Текущее состояние соединения.
    GetConnectionState {},
    /// Дождаться состояния `connected`.
    WaitOnline {
        /// Тайм-аут в миллисекундах, по умолчанию 5000, максимум 600000.
        #[schemars(range(max = 600000))]
        timeout_ms: Option<u64>,
    },
    /// Текущая сессия или `null`.
    GetSession {},
    /// Локальный аккаунт или `null`.
    GetAccount {},
    /// Сводный статус клиента.
    GetStatus {},
    /// Регистрация и вход.
    Register {
        username: String,
        display_name: Option<String>,
        password: String,
    },
    /// Вход в существующий аккаунт.
    Login { username: String, password: String },
    /// Выход; `wipe` удаляет и локальные данные.
    Logout { wipe: Option<bool> },
    /// Чаты из локального кеша.
    GetChats {},
    /// Чаты с сервера (сеть).
    RefreshChats {},
    /// Участники чата из локального кеша.
    GetChatMembers { chat_id: String },
    /// Просить собеседника перевести личный чат в приватный режим (E2E).
    RequestPrivateMode { chat_id: String },
    /// Отозвать собственный запрос на приватный режим.
    CancelPrivateMode { chat_id: String },
    /// Ответить на запрос: принять (перевести чат в E2E) или отклонить.
    ResolvePrivateMode {
        chat_id: String,
        /// `true` — принять, `false` — отклонить.
        accept: bool,
    },
    /// Поиск пользователей по префиксу username (для выбора собеседника).
    SearchUsers {
        /// Префикс username (регистр не важен).
        query: String,
        /// Максимум результатов; по умолчанию 20.
        limit: Option<u32>,
    },
    /// Настройки клиента (режим чатов по умолчанию).
    GetSettings {},
    /// Сменить режим чатов по умолчанию: `cloud` или `private`.
    SetDefaultChatMode {
        /// `cloud` — облачный режим (по умолчанию), `private` — сквозное
        /// шифрование, `none` — без шифрования (только для разработки).
        mode: String,
    },
    /// Поиск по сообщениям аккаунта (облачный режим: сервер расшифровывает
    /// содержимое ключом чата).
    SearchMessages {
        /// Строка запроса.
        query: String,
        /// Ограничение числа ответов; по умолчанию 50.
        limit: Option<u32>,
    },
    /// Создать личный чат.
    CreateDirectChat {
        peer_username: String,
        /// Режим чата: `cloud`, `private` или `none`. Если не задан — режим
        /// по умолчанию из настроек.
        mode: Option<String>,
        /// Прежний флаг: `true` — приватный чат, `false` — без шифрования.
        /// Используется, только если не задан `mode`.
        e2e: Option<bool>,
    },
    /// Создать групповой чат.
    CreateGroupChat {
        name: String,
        /// Username-ы приглашённых; по умолчанию пусто.
        members: Option<Vec<String>>,
        /// Режим чата: `cloud`, `private` или `none`. Если не задан — режим
        /// по умолчанию из настроек.
        mode: Option<String>,
        /// Прежний флаг: `true` — приватный чат, `false` — без шифрования.
        /// Используется, только если не задан `mode`.
        e2e: Option<bool>,
    },
    /// Пригласить пользователей в групповой чат.
    InviteToChat {
        chat_id: String,
        usernames: Vec<String>,
    },
    /// Покинуть чат.
    LeaveChat { chat_id: String },
    /// Удалить участника (владелец группы).
    RemoveChatMember { chat_id: String, user_id: String },
    /// Переименовать групповой чат.
    RenameChat { chat_id: String, name: String },
    /// Поставить текстовое сообщение в outbox.
    SendMessage {
        chat_id: String,
        /// Текст сообщения; может быть пустым, если есть вложение.
        text: Option<String>,
        /// Вложение (файл, фото, видео, голос). Едет внутри зашифрованного
        /// payload: сервер не видит содержимое.
        attachment: Option<AttachmentUpload>,
        reply_to_server_id: Option<u64>,
    },
    /// Вернуть упавшее сообщение в очередь отправки (та же запись, без новой
    /// копии: ключ идемпотентности сохраняется).
    RetryMessage {
        /// Локальный идентификатор сообщения (`local_id`).
        local_id: String,
    },
    /// Содержимое вложения сообщения (для отрисовки в UI).
    DownloadAttachment { server_id: u64 },
    /// Локальная история сообщений.
    GetMessages {
        chat_id: String,
        before_server_id: Option<u64>,
        /// 1..=100, по умолчанию 50.
        #[schemars(range(min = 1, max = 100))]
        limit: Option<u64>,
    },
    /// История с сервера.
    FetchHistory {
        chat_id: String,
        before_server_id: Option<u64>,
        /// 1..=100, по умолчанию 50.
        #[schemars(range(min = 1, max = 100))]
        limit: Option<u64>,
    },
    /// Изменить текст сообщения.
    EditMessage {
        chat_id: String,
        server_id: u64,
        /// Непустой текст.
        text: String,
    },
    /// Удалить сообщение.
    DeleteMessage { chat_id: String, server_id: u64 },
    /// Отметить прочитанным до `up_to_server_id` (или до последнего в чате).
    MarkRead {
        chat_id: String,
        up_to_server_id: Option<u64>,
    },
    /// Receipts чата.
    GetReceipts { chat_id: String },
    /// Синхронизировать изменения с сервером.
    Sync {},
    /// Устройства текущего аккаунта (сеть).
    GetDevices {},
    /// Обновить кеш устройств указанных пользователей.
    RefreshDevices { user_ids: Vec<String> },
    /// Отозвать устройство текущего аккаунта.
    RevokeDevice { device_id: String },
    /// Пригласить собеседника в личный чат (только сигнализация).
    CallInvite { chat_id: String },
    /// Принять входящий звонок.
    CallAccept { call_id: String },
    /// Отклонить входящий звонок.
    CallDecline { call_id: String },
    /// Завершить звонок.
    CallEnd { call_id: String },
}

/// Успешный ответ. Поле `data` зависит от команды в `request`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct OkResponse {
    #[serde(rename = "@type")]
    pub type_: OkTag,
    /// `@type` исходного запроса.
    pub request: String,
    pub data: ResponseData,
}

/// Дискриминатор успешного ответа.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub enum OkTag {
    #[serde(rename = "ok")]
    Ok,
}

/// Ошибочный ответ.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ErrorResponse {
    #[serde(rename = "@type")]
    pub type_: ErrorTag,
    /// `@type` исходного запроса, если он был разобран.
    pub request: Option<String>,
    /// Стабильный код ошибки FFI.
    pub code: FfiErrorCode,
    /// Диагностическое сообщение; текст сервера недоверенный.
    pub message: String,
    /// Имеет ли смысл повторить операцию.
    pub retryable: bool,
    /// Числовой код сервера; только для `code == "server"`.
    pub server_code: Option<i32>,
    /// Имя серверного кода; только для `code == "server"`.
    pub server_code_name: Option<String>,
}

/// Дискриминатор ошибки.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub enum ErrorTag {
    #[serde(rename = "error")]
    Error,
}

/// Стабильные коды ошибок FFI (см. `docs/ffi.md` §6).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FfiErrorCode {
    InvalidRequest,
    InvalidHandle,
    UnknownMethod,
    Server,
    Transport,
    Storage,
    SecretStore,
    Crypto,
    NotAuthenticated,
    Protocol,
    NotFound,
    E2eUnavailable,
    NotConnected,
    Timeout,
    Closed,
    Panic,
    Internal,
}

/// Общий тип `data` успешного ответа.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ResponseData {
    Ping(PingData),
    Connection(ConnectionStateData),
    Session(Session),
    Account(Account),
    Status(Status),
    Logout(LogoutData),
    Chats(Vec<Chat>),
    Members(Vec<ChatMember>),
    SearchUsers(SearchUsersData),
    RequestPrivateMode(RequestPrivateModeData),
    ResolvePrivateMode(ResolvePrivateModeData),
    Chat(Chat),
    SendMessage(SendMessageData),
    RetryMessage(RetryMessageData),
    DownloadAttachment(DownloadAttachmentData),
    Messages(Vec<Message>),
    Message(Message),
    MarkRead(MarkReadData),
    Receipts(Vec<Receipt>),
    Sync(SyncProgress),
    Devices(Vec<Device>),
    Call(Call),
    Null(NullData),
}

/// `data: null` для команд без полезной нагрузки.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct NullData;

/// `ping`: версии библиотеки и JSON-схемы.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PingData {
    pub service: String,
    pub version: String,
    pub api_version: u32,
}

/// `{state}` для команд состояния и события соединения.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ConnectionStateData {
    pub state: ConnectionState,
}

/// Состояние соединения.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Unauthenticated,
    Connected,
    Reconnecting,
}

/// Ответ `logout`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct LogoutData {
    pub wiped: bool,
}

/// Ответ `send_message`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SendMessageData {
    pub local_id: String,
}

/// Ответ `retry_message`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RetryMessageData {
    /// `true` — сообщение возвращено в очередь; `false` — такой записи нет
    /// или она уже отправлена.
    pub requeued: bool,
}

/// Ответ `mark_read`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MarkReadData {
    pub up_to_server_id: u64,
}

/// Визуальные метаданные аватара.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Avatar {
    /// 1–2 буквы в верхнем регистре; пустая строка, если букв нет.
    #[schemars(length(max = 2))]
    pub initials: String,
    /// Индекс цвета в палитре из 8 цветов (`0..=7`).
    #[schemars(range(max = 7))]
    pub color_index: u8,
    /// HEX-код цвета, например `#65AADD`.
    pub color_hex: String,
}

/// Объект сессии.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Session {
    pub active: bool,
    pub user_id: String,
    pub username: String,
    pub display_name: String,
    pub device_id: Option<String>,
    pub avatar: Avatar,
}

/// Локальный аккаунт (объект пользователя).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Account {
    pub user_id: String,
    pub username: String,
    pub display_name: String,
    pub device_id: String,
    pub avatar: Avatar,
}

/// Сводный статус клиента.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Status {
    pub connection: ConnectionState,
    pub session: Option<Session>,
    pub account: Option<Account>,
    pub sync_cursor: u64,
    pub outbox: OutboxStats,
    pub data_dir: String,
    pub server: String,
    pub device_name: String,
    pub e2e: bool,
}

/// Статистика outbox.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct OutboxStats {
    pub total: u64,
    pub due: u64,
}

/// Объект чата.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Chat {
    pub chat_id: String,
    pub kind: ChatKind,
    pub e2e_mode: E2eMode,
    pub title: String,
    pub peer_user_id: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub last_server_id: u64,
    /// Превью последнего сообщения чата (для списка чатов).
    pub last_message: Option<ChatPreview>,
    pub avatar: Avatar,
}

/// Превью последнего сообщения чата.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ChatPreview {
    /// Текст сообщения (пусто, если сообщение — вложение).
    pub body: Option<String>,
    /// Отправитель (для префикса «Вы:»).
    pub sender_user_id: String,
    /// Вид содержимого: `text`, `photo`, `video`, `file`, `voice`.
    pub content_kind: String,
    /// Вид вложения, если сообщение с вложением.
    pub attachment_kind: Option<String>,
    pub created_at_ms: u64,
}

/// Тип чата.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChatKind {
    Unspecified,
    Direct,
    Group,
}

/// Режим E2E чата.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub enum E2eMode {
    #[serde(rename = "olm_v1")]
    OlmV1,
    #[serde(rename = "none_dev")]
    NoneDev,
    #[serde(rename = "unspecified")]
    Unspecified,
}

/// Ответ `request_private_mode`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RequestPrivateModeData {
    /// `true` — запрос отправлен и ждёт подтверждения.
    pub pending: bool,
}

/// Ответ `resolve_private_mode`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ResolvePrivateModeData {
    /// `true` — чат переведён в приватный режим.
    pub switched: bool,
}

/// Ответ `search_users`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SearchUsersData {
    pub users: Vec<UserSummary>,
}

/// Краткая карточка пользователя (результат поиска).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UserSummary {
    pub user_id: String,
    pub username: String,
    pub display_name: String,
    pub avatar: Avatar,
}

/// Участник чата.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ChatMember {
    pub user_id: String,
    pub username: String,
    pub display_name: String,
    pub role: MemberRole,
    pub joined_at_ms: u64,
    pub avatar: Avatar,
}

/// Роль участника.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemberRole {
    Unspecified,
    Member,
    Owner,
}

/// Вложение сообщения при отправке (содержимое в base64).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AttachmentUpload {
    /// Вид: `photo`, `video`, `file`, `voice`.
    pub kind: String,
    /// Имя файла.
    pub name: String,
    /// MIME-тип.
    pub mime: String,
    /// Размер в байтах.
    pub size: u64,
    /// Содержимое файла в base64.
    pub data_base64: String,
    /// Постер-кадр видео (base64) — уходит отдельным блобом.
    pub thumb_base64: Option<String>,
    /// MIME-тип постера.
    pub thumb_mime: Option<String>,
    /// Пики звуковой волны голосового (base64) — для отрисовки в UI.
    pub waveform_base64: Option<String>,
}

/// Метаданные вложения сообщения (без содержимого).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Attachment {
    /// Вид: `photo`, `video`, `file`, `voice`.
    pub kind: String,
    /// Имя файла.
    pub name: String,
    /// MIME-тип.
    pub mime: String,
    /// Размер в байтах.
    pub size: u64,
    /// Наличие постера-кадра (видео).
    pub thumb: Option<AttachmentThumb>,
    /// Пики звуковой волны (голосовое).
    pub waveform: Option<String>,
}

/// Метаданные постера-кадра (видео).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AttachmentThumb {
    /// Вид: всегда `photo`.
    pub kind: String,
    /// Имя файла.
    pub name: String,
    /// MIME-тип.
    pub mime: String,
    /// Размер в байтах.
    pub size: u64,
}

/// Ответ `download_attachment`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DownloadAttachmentData {
    /// Вид: `photo`, `video`, `file`, `voice`.
    pub kind: String,
    /// Имя файла.
    pub name: String,
    /// MIME-тип.
    pub mime: String,
    /// Размер в байтах.
    pub size: u64,
    /// Содержимое файла в base64.
    pub data_base64: String,
}

/// Объект сообщения.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Message {
    pub local_id: String,
    pub server_id: Option<u64>,
    pub chat_id: String,
    pub sender_user_id: String,
    pub sender_device_id: String,
    /// Например, `text`, `photo`, `video`, `file`, `voice`.
    pub content_kind: String,
    pub body: Option<String>,
    /// Метаданные вложения; содержимое — через `download_attachment`.
    pub attachment: Option<Attachment>,
    /// Идентификатор альбома (медиагруппы); сообщения с одинаковым id —
    /// один альбом.
    pub grouped_id: Option<String>,
    pub reply_to_server_id: Option<u64>,
    pub status: MessageStatus,
    pub created_at_ms: u64,
    pub edited_at_ms: Option<u64>,
    pub deleted_at_ms: Option<u64>,
}

/// Статус сообщения в outbox/истории.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub enum MessageStatus {
    #[serde(rename = "none")]
    NoneStatus,
    Pending,
    Sending,
    Sent,
    Failed,
}

/// Receipt доставки/прочтения.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Receipt {
    pub chat_id: String,
    pub user_id: String,
    pub delivered_up_to: u64,
    pub read_up_to: u64,
    pub updated_at_ms: u64,
}

/// Устройство пользователя (только публичные ключи).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Device {
    pub device_id: String,
    pub user_id: String,
    pub name: String,
    pub platform: String,
    pub e2e_version: u32,
    /// Hex-кодированный публичный identity-ключ.
    pub ed25519: String,
    /// Hex-кодированный публичный curve25519-ключ.
    pub curve25519: String,
    pub revoked_at_ms: Option<u64>,
}

/// Прогресс синхронизации.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SyncProgress {
    pub from_seq: u64,
    pub to_seq: u64,
    pub events_applied: u64,
    pub events_skipped: u64,
    pub resync_performed: bool,
}

/// Сигнализация звонка (медиа в v1 нет).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Call {
    pub call_id: String,
    pub chat_id: String,
    pub caller_user_id: String,
    pub callee_user_id: String,
    pub state: CallState,
    pub created_at_ms: u64,
    pub accepted_at_ms: Option<u64>,
}

/// Состояние звонка.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CallState {
    Ringing,
    Active,
    Unknown,
}

/// Событие очереди `{"@type":"event","event":"...","data":...}`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Event {
    ConnectionStateChanged(ConnectionStateChangedEvent),
    SessionStarted(SessionStartedEvent),
    SessionEnded(SessionEndedEvent),
    MessageReceived(MessageReceivedEvent),
    MessageUpdated(MessageUpdatedEvent),
    OutboxChanged(OutboxChangedEvent),
    ChatUpserted(ChatUpsertedEvent),
    ReceiptUpdated(ReceiptUpdatedEvent),
    PresenceChanged(PresenceChangedEvent),
    CallInvited(CallInvitedEvent),
    CallAccepted(CallAcceptedEvent),
    CallDeclined(CallDeclinedEvent),
    CallEnded(CallEndedEvent),
    SyncProgress(SyncProgressEvent),
    ResyncPerformed(ResyncPerformedEvent),
    E2eDecryptFailed(E2eDecryptFailedEvent),
    Warning(WarningEvent),
    EventLagged(EventLaggedEvent),
}

/// Дискриминатор конверта события.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub enum EventTag {
    #[serde(rename = "event")]
    Event,
}

macro_rules! event_struct {
    ($struct:ident, $name:ident, $wire:literal, $data:ty, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Serialize, Deserialize, JsonSchema)]
        pub enum $name {
            #[serde(rename = $wire)]
            Value,
        }

        #[doc = $doc]
        #[derive(Debug, Serialize, Deserialize, JsonSchema)]
        pub struct $struct {
            #[serde(rename = "@type")]
            pub type_: EventTag,
            #[serde(rename = "event")]
            pub event: $name,
            pub data: $data,
        }
    };
}

event_struct!(
    ConnectionStateChangedEvent,
    ConnectionStateChangedName,
    "connection_state_changed",
    ConnectionStateData,
    "Изменилось состояние соединения."
);
event_struct!(
    SessionStartedEvent,
    SessionStartedName,
    "session_started",
    SessionStartedData,
    "Сессия активна после login/resume."
);
event_struct!(
    SessionEndedEvent,
    SessionEndedName,
    "session_ended",
    SessionEndedData,
    "Сессия завершена."
);
event_struct!(
    PrivateModeRequestedEvent,
    PrivateModeRequestedName,
    "private_mode_requested",
    PrivateModeRequestedData,
    "Собеседник предлагает включить приватный режим (E2E)."
);
event_struct!(
    PrivateModeCancelledEvent,
    PrivateModeCancelledName,
    "private_mode_cancelled",
    PrivateModeCancelledData,
    "Запрос на приватный режим отозван."
);
event_struct!(
    PrivateModeResolvedEvent,
    PrivateModeResolvedName,
    "private_mode_resolved",
    PrivateModeResolvedData,
    "Запрос на приватный режим разрешён или отклонён."
);
event_struct!(
    MessageReceivedEvent,
    MessageReceivedName,
    "message_received",
    Message,
    "Новое сообщение."
);
event_struct!(
    MessageUpdatedEvent,
    MessageUpdatedName,
    "message_updated",
    Message,
    "Сообщение изменено (edit/delete)."
);
event_struct!(
    OutboxChangedEvent,
    OutboxChangedName,
    "outbox_changed",
    OutboxChangedData,
    "Изменилось состояние отправки собственного сообщения."
);
event_struct!(
    ChatUpsertedEvent,
    ChatUpsertedName,
    "chat_upserted",
    Chat,
    "Чат создан или обновлён."
);
event_struct!(
    ReceiptUpdatedEvent,
    ReceiptUpdatedName,
    "receipt_updated",
    Receipt,
    "Обновился receipt."
);
event_struct!(
    PresenceChangedEvent,
    PresenceChangedName,
    "presence_changed",
    PresenceChangedData,
    "Изменился online-статус пользователя."
);
event_struct!(
    CallInvitedEvent,
    CallInvitedName,
    "call_invited",
    Call,
    "Входящий звонок (только callee)."
);
event_struct!(
    CallAcceptedEvent,
    CallAcceptedName,
    "call_accepted",
    Call,
    "Звонок принят."
);
event_struct!(
    CallDeclinedEvent,
    CallDeclinedName,
    "call_declined",
    Call,
    "Звонок отклонён."
);
event_struct!(
    CallEndedEvent,
    CallEndedName,
    "call_ended",
    Call,
    "Звонок завершён."
);
event_struct!(
    SyncProgressEvent,
    SyncProgressName,
    "sync_progress",
    SyncProgress,
    "Прогресс синхронизации."
);
event_struct!(
    ResyncPerformedEvent,
    ResyncPerformedName,
    "resync_performed",
    ResyncPerformedData,
    "Выполнен snapshot после `ResyncRequired`."
);
event_struct!(
    E2eDecryptFailedEvent,
    E2eDecryptFailedName,
    "e2e_decrypt_failed",
    E2eDecryptFailedData,
    "Не удалось расшифровать E2E-сообщение."
);
event_struct!(
    WarningEvent,
    WarningName,
    "warning",
    WarningData,
    "Некритичное предупреждение."
);
event_struct!(
    EventLaggedEvent,
    EventLaggedName,
    "event_lagged",
    EventLaggedData,
    "Очередь событий переполнилась; часть событий потеряна."
);

/// `data` события `session_started`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SessionStartedData {
    pub user_id: String,
    pub username: String,
}

/// `data` события `private_mode_requested`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PrivateModeRequestedData {
    pub chat_id: String,
    pub requester_user_id: String,
    pub requester_username: String,
    pub requester_display_name: String,
}

/// `data` события `private_mode_cancelled`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PrivateModeCancelledData {
    pub chat_id: String,
}

/// `data` события `private_mode_resolved`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PrivateModeResolvedData {
    pub chat_id: String,
    /// `true` — чат переведён в приватный режим.
    pub accepted: bool,
}

/// `data` события `session_ended`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SessionEndedData {
    pub reason: String,
}

/// `data` события `outbox_changed`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct OutboxChangedData {
    pub local_id: String,
    pub state: OutboxState,
    pub error: Option<String>,
    /// Код ошибки сервера (если ошибка пришла от сервера).
    pub error_code: Option<i32>,
}

/// Состояние отправки сообщения.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutboxState {
    Pending,
    Sending,
    Sent,
    Failed,
}

/// `data` события `presence_changed`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PresenceChangedData {
    pub user_id: String,
    pub online: bool,
}

/// `data` события `resync_performed`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ResyncPerformedData {
    pub up_to_seq: u64,
}

/// `data` события `e2e_decrypt_failed`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct E2eDecryptFailedData {
    pub chat_id: String,
    pub server_id: u64,
    pub device_id: String,
}

/// `data` события `warning`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct WarningData {
    pub message: String,
}

/// `data` события `event_lagged`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct EventLaggedData {
    pub missed: u64,
}

/// Сериализует полную схему протокола (с завершающим переводом строки).
pub fn schema_json() -> Result<String, serde_json::Error> {
    let mut text = serde_json::to_string_pretty(&schemars::schema_for!(RchatMessage))?;
    text.push('\n');
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_contains_all_commands_and_events() {
        let text = schema_json().expect("schema json");
        for method in [
            "ping",
            "start",
            "get_connection_state",
            "wait_online",
            "get_session",
            "get_account",
            "get_status",
            "register",
            "login",
            "logout",
            "get_chats",
            "refresh_chats",
            "get_chat_members",
            "search_users",
            "request_private_mode",
            "cancel_private_mode",
            "resolve_private_mode",
            "get_settings",
            "set_default_chat_mode",
            "search_messages",
            "create_direct_chat",
            "create_group_chat",
            "invite_to_chat",
            "leave_chat",
            "remove_chat_member",
            "rename_chat",
            "send_message",
            "retry_message",
            "download_attachment",
            "get_messages",
            "fetch_history",
            "edit_message",
            "delete_message",
            "mark_read",
            "get_receipts",
            "sync",
            "get_devices",
            "refresh_devices",
            "revoke_device",
            "call_invite",
            "call_accept",
            "call_decline",
            "call_end",
        ] {
            assert!(
                text.contains(&format!("\"{method}\"")),
                "command {method} missing from schema"
            );
        }
        for event in [
            "connection_state_changed",
            "session_started",
            "session_ended",
            "message_received",
            "message_updated",
            "outbox_changed",
            "chat_upserted",
            "receipt_updated",
            "presence_changed",
            "call_invited",
            "call_accepted",
            "call_declined",
            "call_ended",
            "sync_progress",
            "resync_performed",
            "e2e_decrypt_failed",
            "warning",
            "event_lagged",
        ] {
            assert!(
                text.contains(&format!("\"{event}\"")),
                "event {event} missing from schema"
            );
        }
    }
}

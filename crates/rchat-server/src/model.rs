//! Доменные записи серверной стороны.
//!
//! Типы не содержат SQL-специфики и не зависят от `prost`: это контракт
//! между прикладной логикой (`rchat-server`) и реализациями `Repo`.
//! Времена — миллисекунды Unix, 0 означает «не задано».

/// Пользователь.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRecord {
    pub user_id: String,
    pub username: String,
    pub display_name: String,
    /// PHC-строка Argon2id (или другого хешера) — не секрет, но и не для логов.
    pub password_hash: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub disabled: bool,
}

/// Устройство пользователя. Публичные ключи E2E; приватных ключей сервер не
/// видит и не хранит.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRecord {
    pub device_id: String,
    pub user_id: String,
    pub name: String,
    pub platform: String,
    /// 0 — без E2E-ключей, 1 — olm.v1.
    pub e2e_version: u32,
    /// Пусто, если E2E не настроен.
    pub ed25519: Vec<u8>,
    pub curve25519: Vec<u8>,
    pub created_at_ms: u64,
    pub last_seen_at_ms: u64,
    /// 0 — активно.
    pub revoked_at_ms: u64,
}

impl DeviceRecord {
    pub fn is_revoked(&self) -> bool {
        self.revoked_at_ms != 0
    }

    pub fn has_e2e_keys(&self) -> bool {
        self.e2e_version != 0 && !self.ed25519.is_empty() && !self.curve25519.is_empty()
    }
}

/// Новое устройство при регистрации/входе.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDevice {
    pub device_id: String,
    pub name: String,
    pub platform: String,
    pub e2e_version: u32,
    pub ed25519: Vec<u8>,
    pub curve25519: Vec<u8>,
    pub one_time_keys: Vec<PreKeyRecord>,
    pub fallback_key: Option<PreKeyRecord>,
}

/// Сессия входа. Хранится только хеш токена.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub session_id: String,
    /// SHA-256 от непрозрачного токена (сам токен сервер не хранит).
    pub token_hash: Vec<u8>,
    pub user_id: String,
    pub device_id: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub last_used_at_ms: u64,
    pub revoked_at_ms: u64,
}

impl SessionRecord {
    pub fn is_valid(&self, now_ms: u64) -> bool {
        self.revoked_at_ms == 0 && self.expires_at_ms > now_ms
    }
}

/// Новая сессия. `session_id` и `last_used_at` назначает хранилище.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    pub token_hash: Vec<u8>,
    pub device_id: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
}

/// Регистрация: пользователь + первое устройство + сессия — атомарно.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewUser {
    pub username: String,
    pub display_name: String,
    pub password_hash: String,
    pub created_at_ms: u64,
}

/// Публичный ключ устройства, ожидающий выдачи в пакете (one-time или fallback).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreKeyRecord {
    pub key_id: u32,
    pub public_key: Vec<u8>,
    /// true — резервный ключ (не расходуется).
    pub fallback: bool,
    pub created_at_ms: u64,
}

/// Пакет для установления сессии: устройство + выделенный предключ (если есть).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleRecord {
    pub device: DeviceRecord,
    pub one_time_key: Option<PreKeyRecord>,
    /// true, если `one_time_key` — резервный.
    pub fallback: bool,
}

/// Чат.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRecord {
    pub chat_id: String,
    /// Значение `proto::ChatKind` (1 — direct, 2 — group).
    pub kind: i32,
    /// Значение `proto::E2eMode`.
    pub e2e_mode: i32,
    /// Ключ облачного чата (32 байта); пусто в E2E-чатах. Сервер знает его
    /// и поэтому может расшифровывать содержимое (облачный режим).
    pub cloud_key: Vec<u8>,
    /// Название группового чата; пусто для личных.
    pub name: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub last_server_id: u64,
}

/// Метаданные загружаемого файла (облачный режим).
#[derive(Debug, Clone, Default)]
pub struct BlobMeta {
    /// Файл зашифрован ключом аккаунта (сервер знает ключ).
    pub cloud: bool,
    /// Отпечаток открытого содержимого (SHA-256) — для дедупликации.
    pub content_hash: Vec<u8>,
    /// Nonce шифрования файла.
    pub nonce: Vec<u8>,
    /// Имя файла.
    pub name: String,
    /// Вид вложения: photo / video / file / voice.
    pub kind: String,
    /// MIME-тип.
    pub mime: String,
    /// Размер открытого содержимого.
    pub plaintext_size: u64,
}

/// Участник чата.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRecord {
    pub chat_id: String,
    pub user_id: String,
    pub joined_at_ms: u64,
    /// Значение `proto::ChatMemberRole` (0 — direct/не задано, 1 — member,
    /// 2 — owner). У группы ровно один owner, пока в ней есть участники.
    pub role: i32,
}

impl MemberRecord {
    pub fn is_owner(&self) -> bool {
        self.role == rchat_proto::proto::ChatMemberRole::Owner as i32
    }
}

/// Сообщение (без получательских ciphertext-ов).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRecord {
    pub server_id: u64,
    pub chat_id: String,
    pub sender_user_id: String,
    pub sender_device_id: String,
    pub client_message_id: String,
    pub content_kind: String,
    pub reply_to_server_id: u64,
    pub created_at_ms: u64,
    pub edited_at_ms: u64,
    pub deleted_at_ms: u64,
    pub sender_curve25519: Vec<u8>,
    pub olm_type: i32,
    /// Только для чатов `E2E_MODE_NONE_DEV`.
    pub plaintext_dev: Option<String>,
    /// Megolm-ciphertext группового E2E-сообщения (общий для всех устройств).
    pub group_ciphertext: Option<Vec<u8>>,
    /// Идентификатор Megolm-сессии (base64); пусто, если не применимо.
    pub group_session_id: String,
    /// Идентификатор альбома (медиагруппы); пусто для обычных сообщений.
    pub grouped_id: String,
    /// Облачный режим: содержимое под ключом аккаунта (сервер знает ключ).
    pub cloud_ciphertext: Vec<u8>,
    /// Nonce облачного шифра.
    pub cloud_nonce: Vec<u8>,
}

/// Ciphertext сообщения для одного устройства получателя.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientRecord {
    pub server_id: u64,
    pub device_id: String,
    pub ciphertext: Vec<u8>,
    pub e2e_version: u32,
    pub olm_type: i32,
    pub sender_curve25519: Vec<u8>,
    /// Значение `proto::RecipientPayloadKind` (0 — содержимое, 1 — ключ
    /// групповой Megolm-сессии).
    pub payload_kind: i32,
}

/// Событие журнала синхронизации устройства.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncEventRecord {
    pub device_id: String,
    pub seq: u64,
    pub kind_hint: u32,
    /// Сериализованный `proto::SyncEvent` (поле `seq` может быть нулевым).
    pub payload: Vec<u8>,
    pub created_at_ms: u64,
}

/// Новое событие журнала (seq назначает хранилище).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSyncEvent {
    pub device_id: String,
    pub kind_hint: u32,
    pub payload: Vec<u8>,
    pub created_at_ms: u64,
}

/// Страница журнала синхронизации.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncFetchRecord {
    pub events: Vec<SyncEventRecord>,
    /// Максимальный seq журнала устройства на момент чтения.
    pub up_to_seq: u64,
    /// Минимальный хранимый seq (0, если журнал пуст).
    pub min_seq: u64,
    /// true — между курсором клиента и началом журнала есть потерянные события.
    pub resync_required: bool,
}

/// Итог вставки сообщения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertMessageOutcome {
    /// Сообщение создано; `event_seqs` — назначенные seq по устройствам.
    Inserted {
        message: MessageRecord,
        event_seqs: Vec<(String, u64)>,
    },
    /// Повтор с тем же idempotency key: возвращается результат первой операции.
    Duplicate(MessageRecord),
}

/// Вставка сообщения с recipients и событиями (одна транзакция).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMessage {
    pub message: MessageRecord,
    pub recipients: Vec<RecipientRecord>,
    pub events: Vec<NewSyncEvent>,
}

/// Обновление сообщения (редактирование/удаление) с событиями.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageUpdate {
    /// Установленные поля: edited_at_ms и/или deleted_at_ms, content_kind.
    pub edited_at_ms: u64,
    pub deleted_at_ms: u64,
    pub content_kind: Option<String>,
    /// Новое значение `plaintext_dev` (только dev-чаты); `None` — не менять.
    pub plaintext_dev: Option<String>,
    /// Новый megolm-ciphertext группового E2E-сообщения; `None` — не менять.
    pub group_ciphertext: Option<Vec<u8>>,
    /// Новый идентификатор Megolm-сессии; `None` — не менять.
    pub group_session_id: Option<String>,
    /// Удаление для всех: содержимое стирается (`plaintext_dev` → NULL,
    /// `group_ciphertext` → NULL, `group_session_id` → '', recipients
    /// заменяются на пустой список).
    pub clear_content: bool,
    /// Новый `sender_device_id` (правка с другого устройства автора);
    /// `None` — не менять.
    pub sender_device_id: Option<String>,
    /// Новый identity-ключ устройства-редактора; `None` — не менять.
    pub sender_curve25519: Option<Vec<u8>>,
    /// Новые ciphertext-ы для устройств; `None` — оставить существующие.
    pub recipients: Option<Vec<RecipientRecord>>,
    pub events: Vec<NewSyncEvent>,
    /// Пользователь, от имени которого выполняется изменение.
    pub editor_user_id: String,
}

/// Подтверждение доставки/прочтения (последнее состояние по (чат, пользователь)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptRecord {
    pub chat_id: String,
    pub user_id: String,
    pub up_to_server_id: u64,
    pub read: bool,
    pub updated_at_ms: u64,
}

/// Результат обновления receipt: изменилось ли сохранённое состояние.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceiptUpdate {
    pub moved: bool,
    pub up_to_server_id: u64,
    pub read: bool,
}

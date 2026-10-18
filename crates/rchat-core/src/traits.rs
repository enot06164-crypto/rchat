//! Контракты внешних зависимостей ядра. Конкретные реализации
//! (WebSocket, SQLite, криптография, системное хранилище секретов)
//! поставляются адаптерами и собираются в composition root приложения.

use async_trait::async_trait;

use crate::error::{CryptoError, SecretStoreError, StorageError, TransportError};
use crate::types::{
    ChatRecord, DeviceRecord, LocalAccount, MemberRecord, MessageRecord, OutboxRecord, OutboxState,
    ReceiptRecord, SyncApply, UserRecord,
};

// ---------------------------------------------------------------------------
// Транспорт
// ---------------------------------------------------------------------------

/// Один кадр сетевого обмена — сериализованный `Envelope`.
/// Транспорт не знает о содержимом кадров.
#[async_trait]
pub trait Transport: Send {
    /// Отправляет кадр. Ошибка означает потерю соединения.
    async fn send(&mut self, frame: &[u8]) -> Result<(), TransportError>;
    /// Ожидает следующий кадр. `Ok(None)` — корректное закрытие соединения.
    async fn recv(&mut self) -> Result<Option<Vec<u8>>, TransportError>;
    /// Закрывает соединение; идемпотентно.
    async fn close(&mut self) -> Result<(), TransportError>;
}

/// Фабрика транспорта: создаёт новое соединение на каждый цикл реконнекта.
#[async_trait]
pub trait TransportFactory: Send + Sync {
    async fn connect(&self) -> Result<Box<dyn Transport>, TransportError>;
}

// ---------------------------------------------------------------------------
// Хранилище клиента
// ---------------------------------------------------------------------------

/// Локальное хранилище клиента. Все методы могут вызываться конкурентно;
/// реализация обязана обеспечивать атомарность транзакций, явно указанных
/// в контракте.
#[async_trait]
pub trait Storage: Send + Sync {
    /// Приводит схему к актуальной версии. Идемпотентно.
    async fn migrate(&self) -> Result<(), StorageError>;

    // --- аккаунт ---
    async fn save_account(&self, account: &LocalAccount) -> Result<(), StorageError>;
    async fn load_account(&self) -> Result<Option<LocalAccount>, StorageError>;
    /// Удаляет профиль, чаты, сообщения, outbox и e2e-состояние (logout --wipe).
    async fn clear_all(&self) -> Result<(), StorageError>;

    // --- пользователи и устройства ---
    async fn upsert_user(&self, user: &UserRecord) -> Result<(), StorageError>;
    async fn get_user(&self, user_id: &str) -> Result<Option<UserRecord>, StorageError>;
    async fn list_users(&self) -> Result<Vec<UserRecord>, StorageError>;
    async fn upsert_device(&self, device: &DeviceRecord) -> Result<(), StorageError>;
    async fn get_device(&self, device_id: &str) -> Result<Option<DeviceRecord>, StorageError>;
    async fn list_devices_of(&self, user_id: &str) -> Result<Vec<DeviceRecord>, StorageError>;
    async fn delete_device(&self, device_id: &str) -> Result<(), StorageError>;

    // --- чаты ---
    async fn upsert_chat(
        &self,
        chat: &ChatRecord,
        members: &[MemberRecord],
    ) -> Result<(), StorageError>;
    /// Полная замена списка чатов и участников (snapshot после resync).
    /// Сообщения не затрагиваются.
    async fn replace_chats(
        &self,
        chats: &[(ChatRecord, Vec<MemberRecord>)],
    ) -> Result<(), StorageError>;
    async fn list_chats(&self) -> Result<Vec<ChatRecord>, StorageError>;
    async fn get_chat(&self, chat_id: &str) -> Result<Option<ChatRecord>, StorageError>;
    async fn chat_members(&self, chat_id: &str) -> Result<Vec<MemberRecord>, StorageError>;

    // --- сообщения ---
    async fn get_message(&self, local_id: &str) -> Result<Option<MessageRecord>, StorageError>;
    async fn get_message_by_server(
        &self,
        server_id: u64,
    ) -> Result<Option<MessageRecord>, StorageError>;
    /// Сообщения чата, новые первыми. `before_server_id` — пагинация вниз.
    async fn list_messages(
        &self,
        chat_id: &str,
        before_server_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<MessageRecord>, StorageError>;

    // --- outbox ---
    /// Атомарно сохраняет локальное сообщение и запись outbox.
    async fn insert_outgoing(
        &self,
        message: &MessageRecord,
        outbox: &OutboxRecord,
    ) -> Result<(), StorageError>;
    async fn get_outbox(&self, local_id: &str) -> Result<Option<OutboxRecord>, StorageError>;
    async fn update_outbox(&self, outbox: &OutboxRecord) -> Result<(), StorageError>;
    async fn delete_outbox(&self, local_id: &str) -> Result<(), StorageError>;
    /// Записи, готовые к отправке (`next_attempt_at_ms <= now`), кроме `Sent`.
    async fn due_outbox(&self, now_ms: u64, limit: u32) -> Result<Vec<OutboxRecord>, StorageError>;
    /// Сбрасывает `Sending` в `Pending` (вызывается при разрыве соединения:
    /// подтверждение могло не дойти, повторим с тем же ключом идемпотентности).
    async fn reset_inflight_outbox(&self) -> Result<(), StorageError>;
    /// Подтверждение отправки: статус `Sent`, `server_id`, удаление записи
    /// outbox и обновление `last_server_id` чата — одной транзакцией.
    async fn apply_send_ack(&self, local_id: &str, server_id: u64) -> Result<(), StorageError>;
    /// Ошибка попытки: новый статус сообщения и outbox, следующая попытка.
    async fn apply_outbox_failure(
        &self,
        local_id: &str,
        state: OutboxState,
        error_code: Option<i32>,
        error: &str,
        next_attempt_at_ms: u64,
    ) -> Result<(), StorageError>;

    // --- синхронизация ---
    /// Последний надёжно применённый seq.
    async fn get_sync_cursor(&self) -> Result<u64, StorageError>;
    async fn set_sync_cursor(&self, seq: u64) -> Result<(), StorageError>;
    /// Атомарно применяет пачку мутаций и продвигает курсор до `new_cursor`.
    /// Сбой не должен продвигать курсор.
    async fn apply_sync_batch(
        &self,
        batch: &[SyncApply],
        new_cursor: u64,
    ) -> Result<(), StorageError>;

    // --- receipts ---
    async fn get_receipts(&self, chat_id: &str) -> Result<Vec<ReceiptRecord>, StorageError>;
}

/// Состояние E2E-сессий. Ключи — стабильные строки вида `olm_session:<device_id>`,
/// `olm_account`; значения — сериализованное (и защищённое) состояние,
/// формат которого определяет провайдер криптографии.
#[async_trait]
pub trait CryptoStateStore: Send + Sync {
    async fn crypto_load(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError>;
    async fn crypto_save(&self, key: &str, value: &[u8]) -> Result<(), StorageError>;
    async fn crypto_delete(&self, key: &str) -> Result<(), StorageError>;
    /// Удаляет все ключи с данным префиксом; возвращает число удалённых.
    async fn crypto_delete_prefix(&self, prefix: &str) -> Result<u64, StorageError>;
}

/// Полный контракт локального хранилища клиента.
pub trait ClientStore: Storage + CryptoStateStore {}
impl<T: Storage + CryptoStateStore> ClientStore for T {}

// ---------------------------------------------------------------------------
// Хранилище секретов
// ---------------------------------------------------------------------------

/// Защищённое хранение секретов (токен сессии, ключ шифрования pickles).
/// Значения никогда не логируются и не попадают в обычные файлы.
pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, SecretStoreError>;
    fn set(&self, key: &str, value: &[u8]) -> Result<(), SecretStoreError>;
    fn delete(&self, key: &str) -> Result<(), SecretStoreError>;
}

/// Имена ключей секретов.
pub mod secret_keys {
    pub const SESSION_TOKEN: &str = "session_token";
    pub const CRYPTO_PICKLE_KEY: &str = "crypto_pickle_key";
    /// Постоянный идентификатор устройства. Не секрет, но хранится вместе с
    /// остальными данными профиля и удаляется при `logout --wipe`.
    pub const DEVICE_ID: &str = "device_id";
}

// ---------------------------------------------------------------------------
// Криптография (E2E)
// ---------------------------------------------------------------------------

/// Публичные данные устройства собеседника для установления сессии.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerDevice {
    pub user_id: String,
    pub device_id: String,
    pub ed25519: Vec<u8>,
    pub curve25519: Vec<u8>,
    /// Одноразовый (или резервный) prekey сервера; нужен для создания сессии.
    pub one_time_key: Option<OneTimeKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OneTimeKey {
    pub key_id: u32,
    pub public_key: Vec<u8>,
    pub fallback: bool,
}

impl OneTimeKey {
    pub fn as_proto(&self) -> rchat_proto::proto::PreKey {
        rchat_proto::proto::PreKey {
            key_id: self.key_id,
            public_key: self.public_key.clone(),
        }
    }

    pub fn from_proto(prekey: &rchat_proto::proto::PreKey, fallback: bool) -> Self {
        Self {
            key_id: prekey.key_id,
            public_key: prekey.public_key.clone(),
            fallback,
        }
    }
}

/// Зашифрованная копия сообщения для конкретного устройства.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedMessage {
    pub device_id: String,
    pub ciphertext: Vec<u8>,
    pub e2e_version: u32,
    pub olm_type: rchat_proto::proto::OlmMessageType,
    pub sender_curve25519: Vec<u8>,
}

/// Ключи, готовые к публикации на сервере.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KeyUpload {
    pub one_time_keys: Vec<OneTimeKey>,
    pub fallback_key: Option<OneTimeKey>,
}

/// Контракт E2E-провайдера. Описывает операции уровня протокола
/// (установление сессии, шифрование, расшифрование), а не низкоуровневые
/// примитивы. Реализация сама отвечает за формат и защиту своего состояния.
#[async_trait]
pub trait CryptoProvider: Send + Sync {
    /// Версия протокола (см. `rchat_proto::E2E_VERSION_*`).
    fn e2e_version(&self) -> u32;
    /// Имя протокола для capability negotiation, например "olm.v1".
    fn protocol_name(&self) -> &'static str;
    /// Загружает или создаёт локальное криптографическое состояние.
    async fn initialize(&self) -> Result<(), CryptoError>;
    /// Публичные ключи устройства: (ed25519, curve25519).
    async fn identity_keys(&self) -> Result<(Vec<u8>, Vec<u8>), CryptoError>;
    /// Возвращает ключи для загрузки на сервер, генерируя новые при
    /// необходимости (в том числе резервный ключ).
    async fn keys_for_upload(&self, want_one_time: usize) -> Result<KeyUpload, CryptoError>;
    /// Помечает ключи, выданные последним вызовом `keys_for_upload`, как
    /// опубликованные. Вызывается после подтверждения сервера.
    async fn mark_published(&self) -> Result<(), CryptoError>;
    /// Есть ли установленная сессия с устройством.
    async fn has_session(&self, device_id: &str) -> Result<bool, CryptoError>;
    /// Шифрует текст для каждого целевого устройства. Для устройств без
    /// существующей сессии требует `one_time_key` и создаёт сессию.
    async fn encrypt_for(
        &self,
        targets: &[PeerDevice],
        plaintext: &[u8],
    ) -> Result<Vec<EncryptedMessage>, CryptoError>;
    /// Расшифровывает сообщение, создавая входящую сессию при необходимости.
    async fn decrypt_from(
        &self,
        sender: &PeerDevice,
        message: &EncryptedMessage,
    ) -> Result<Vec<u8>, CryptoError>;
    /// Удаляет сессию (отзыв устройства).
    async fn remove_session(&self, device_id: &str) -> Result<(), CryptoError>;
    /// Полностью удаляет локальное криптографическое состояние.
    async fn wipe(&self) -> Result<(), CryptoError>;
}

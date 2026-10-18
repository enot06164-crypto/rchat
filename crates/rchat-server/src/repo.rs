//! Контракт серверного хранилища.
//!
//! SQL изолирован за этим трейтом (`rchat-server-storage-sqlite`), прикладная
//! логика о SQL не знает. Методы, которые обязаны быть атомарными
//! (регистрация, вставка сообщения с событиями, выдача пакета ключей и т.д.),
//! выполняются реализацией в одной транзакции.
//!
//! Все методы — асинхронные; реализация вправе выполнять блокирующую работу
//! (например, SQLite) в отдельном потоке.
//!
//! Порядок `event_seqs` в результатах совпадает с порядком переданных
//! событий: элемент `i` — назначенный seq для `events[i]`.

use async_trait::async_trait;

use crate::model::{
    BundleRecord, ChatRecord, DeviceRecord, InsertMessageOutcome, MemberRecord, MessageRecord,
    MessageUpdate, NewDevice, NewMessage, NewSession, NewSyncEvent, NewUser, PreKeyRecord,
    ReceiptRecord, ReceiptUpdate, SessionRecord, SyncFetchRecord, UserRecord,
};

/// Ошибка хранилища.
#[derive(Debug, Clone, thiserror::Error)]
pub enum RepoError {
    /// Нарушение уникальности/инварианта. Ключ машиночитаемый:
    /// `username_taken`, `chat_pair`, ...
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("not found: {0}")]
    NotFound(String),
    /// Внутренняя ошибка (детали только в логах).
    #[error("internal: {0}")]
    Internal(String),
}

impl RepoError {
    pub fn internal(error: impl std::fmt::Display) -> Self {
        Self::Internal(error.to_string())
    }
}

#[async_trait]
pub trait Repo: Send + Sync {
    // --- пользователи ---

    /// Регистрация: пользователь + первое устройство (с предключами) + сессия
    /// в одной транзакции.
    async fn create_user_with_device(
        &self,
        user: &NewUser,
        device: &NewDevice,
        session: &NewSession,
    ) -> Result<(UserRecord, SessionRecord), RepoError>;

    async fn find_user_by_username(&self, username: &str) -> Result<Option<UserRecord>, RepoError>;

    async fn find_user_by_id(&self, user_id: &str) -> Result<Option<UserRecord>, RepoError>;

    async fn find_users_by_ids(&self, user_ids: &[String]) -> Result<Vec<UserRecord>, RepoError>;

    // --- устройства ---

    async fn find_device(
        &self,
        user_id: &str,
        device_id: &str,
    ) -> Result<Option<DeviceRecord>, RepoError>;

    /// Поиск устройства по id без известного владельца (для пакетов ключей).
    async fn find_device_by_id(&self, device_id: &str) -> Result<Option<DeviceRecord>, RepoError>;

    /// Создаёт устройство и (при наличии) сохраняет его предключи.
    async fn create_device(
        &self,
        user_id: &str,
        device: &NewDevice,
    ) -> Result<DeviceRecord, RepoError>;

    /// Обновляет E2E-ключи устройства (смена «без ключей» → «olm.v1»).
    async fn update_device_keys(
        &self,
        user_id: &str,
        device_id: &str,
        e2e_version: u32,
        ed25519: &[u8],
        curve25519: &[u8],
    ) -> Result<(), RepoError>;

    async fn touch_device(
        &self,
        user_id: &str,
        device_id: &str,
        now_ms: u64,
    ) -> Result<(), RepoError>;

    /// Помечает устройство отозванным и завершает его сессии.
    async fn revoke_device(
        &self,
        user_id: &str,
        device_id: &str,
        now_ms: u64,
    ) -> Result<DeviceRecord, RepoError>;

    async fn list_devices(
        &self,
        user_id: &str,
        include_revoked: bool,
    ) -> Result<Vec<DeviceRecord>, RepoError>;

    /// Устройства нескольких пользователей (для фан-аута событий).
    async fn devices_for_users(
        &self,
        user_ids: &[String],
        include_revoked: bool,
    ) -> Result<Vec<DeviceRecord>, RepoError>;

    // --- предключи ---

    /// Сохраняет новые предключи устройства; возвращает число добавленных
    /// одноразовых ключей. Дубли по (device, key_id) игнорируются.
    async fn store_prekeys(
        &self,
        user_id: &str,
        device_id: &str,
        keys: &[PreKeyRecord],
        fallback: Option<&PreKeyRecord>,
    ) -> Result<u32, RepoError>;

    /// Атомарно выделяет пакет ключей устройства: один одноразовый предключ
    /// (помечается израсходованным) или резервный, если одноразовых нет.
    async fn take_bundle(
        &self,
        user_id: &str,
        device_id: &str,
    ) -> Result<Option<BundleRecord>, RepoError>;

    // --- сессии ---

    async fn create_session(
        &self,
        user_id: &str,
        session: &NewSession,
    ) -> Result<SessionRecord, RepoError>;

    async fn find_session_by_token_hash(
        &self,
        token_hash: &[u8],
    ) -> Result<Option<SessionRecord>, RepoError>;

    async fn touch_session(&self, session_id: &str, now_ms: u64) -> Result<(), RepoError>;

    async fn revoke_session(&self, session_id: &str, now_ms: u64) -> Result<(), RepoError>;

    // --- чаты ---

    /// Ищет личный чат двух пользователей (в любом порядке участников).
    async fn find_direct_chat(
        &self,
        user_a: &str,
        user_b: &str,
    ) -> Result<Option<(ChatRecord, Vec<MemberRecord>)>, RepoError>;

    /// Создаёт чат, участников и события синхронизации одной транзакцией.
    /// Возвращает назначенные seq в порядке `events`.
    async fn create_chat(
        &self,
        chat: &ChatRecord,
        members: &[MemberRecord],
        events: &[NewSyncEvent],
    ) -> Result<Vec<(String, u64)>, RepoError>;

    async fn get_chat(
        &self,
        chat_id: &str,
    ) -> Result<Option<(ChatRecord, Vec<MemberRecord>)>, RepoError>;

    async fn list_chats_for_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<(ChatRecord, Vec<MemberRecord>)>, RepoError>;

    /// Есть ли у пары пользователей общий чат (личный или групповой).
    /// Используется для видимости устройств (пакеты ключей, GetDevices).
    async fn has_shared_chat(&self, user_a: &str, user_b: &str) -> Result<bool, RepoError>;

    /// Добавляет участников в чат (приглашение): вставка строк, обновление
    /// `updated_at_ms` и события — одной транзакцией. Проверяет предел числа
    /// участников `max_members`; превышение → `Conflict("group_full")`.
    async fn add_chat_members(
        &self,
        chat_id: &str,
        members: &[MemberRecord],
        max_members: usize,
        events: &[NewSyncEvent],
        now_ms: u64,
    ) -> Result<Vec<(String, u64)>, RepoError>;

    /// Удаляет участника (выход или исключение). Если удаляется владелец и
    /// передан `transfer_owner_to`, роль owner переходит этому участнику.
    /// Обновляет `updated_at_ms` и пишет события одной транзакцией.
    async fn remove_chat_member(
        &self,
        chat_id: &str,
        user_id: &str,
        transfer_owner_to: Option<&str>,
        events: &[NewSyncEvent],
        now_ms: u64,
    ) -> Result<Vec<(String, u64)>, RepoError>;

    /// Переименование группового чата; обновляет `updated_at_ms` и пишет
    /// события одной транзакцией.
    async fn rename_chat(
        &self,
        chat_id: &str,
        name: &str,
        events: &[NewSyncEvent],
        now_ms: u64,
    ) -> Result<Vec<(String, u64)>, RepoError>;

    // --- сообщения ---

    /// Вставляет сообщение, recipients, обновляет `chat.last_server_id` и
    /// добавляет события журнала — в одной транзакции. Повтор с тем же
    /// (chat, sender_device, client_message_id) возвращает `Duplicate`
    /// без создания дубликата.
    async fn insert_message(&self, insert: &NewMessage) -> Result<InsertMessageOutcome, RepoError>;

    async fn get_message(&self, server_id: u64) -> Result<Option<MessageRecord>, RepoError>;

    /// Ciphertext сообщения для конкретного устройства (если есть).
    async fn message_recipient(
        &self,
        server_id: u64,
        device_id: &str,
    ) -> Result<Option<crate::model::RecipientRecord>, RepoError>;

    /// Ciphertext-ы пачки сообщений для одного устройства (для страницы истории).
    async fn recipients_for_device(
        &self,
        server_ids: &[u64],
        device_id: &str,
    ) -> Result<Vec<crate::model::RecipientRecord>, RepoError>;

    /// История чата: сообщения строго старше `before_server_id`
    /// (0 — с самых новых), от новых к старым, не более `limit + 1` штук
    /// (лишняя запись означает «есть ещё»).
    async fn list_history(
        &self,
        chat_id: &str,
        before_server_id: u64,
        limit: u32,
    ) -> Result<Vec<MessageRecord>, RepoError>;

    /// Редактирование/удаление: проверяет, что редактор — автор сообщения и
    /// оно не удалено; обновляет поля, recipients и добавляет события.
    /// Возвращает обновлённое сообщение и seq событий в порядке `update.events`.
    async fn update_message(
        &self,
        server_id: u64,
        update: &MessageUpdate,
    ) -> Result<(MessageRecord, Vec<(String, u64)>), RepoError>;

    /// Максимальный выданный server_id (0, если сообщений нет).
    async fn max_message_server_id(&self) -> Result<u64, RepoError>;

    // --- receipts ---

    /// Обновляет receipt (монотонно по up_to_server_id; read не сбрасывается)
    /// и добавляет события для указанных устройств. Возвращает состояние и
    /// seq событий в порядке `events`.
    async fn upsert_receipt(
        &self,
        receipt: &ReceiptRecord,
        events: &[NewSyncEvent],
    ) -> Result<(ReceiptUpdate, Vec<(String, u64)>), RepoError>;

    // --- синхронизация ---

    /// Добавляет события в журналы устройств. Возвращает seq в порядке `events`.
    async fn append_sync_events(
        &self,
        events: &[NewSyncEvent],
    ) -> Result<Vec<(String, u64)>, RepoError>;

    /// Выдаёт страницу журнала устройства после `after_seq`, предварительно
    /// применяя политику хранения: события старше `retention_ms` могут быть
    /// удалены, но у каждого устройства всегда сохраняются `keep_min` последних
    /// событий. `resync_required` — если события после курсора уже удалены.
    async fn fetch_sync_events(
        &self,
        device_id: &str,
        after_seq: u64,
        limit: u32,
        retention_ms: u64,
        keep_min: u32,
        now_ms: u64,
    ) -> Result<SyncFetchRecord, RepoError>;

    /// Запоминает подтверждённый курсор устройства (монотонно).
    async fn record_cursor(
        &self,
        device_id: &str,
        cursor: u64,
        now_ms: u64,
    ) -> Result<(), RepoError>;

    /// Максимальный seq журнала устройства (0, если журнал пуст).
    async fn max_seq_for_device(&self, device_id: &str) -> Result<u64, RepoError>;
}

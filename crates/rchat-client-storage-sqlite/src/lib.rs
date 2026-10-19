//! SQLite-хранилище клиента rchat и резервное хранилище секретов.
//!
//! Крейт реализует контракты ядра [`rchat_core::Storage`] и
//! [`rchat_core::CryptoStateStore`] поверх SQLite, а также
//! [`EncryptedFileSecretStore`] — шифрованное файловое хранилище секретов
//! (см. [`secrets`]).
//!
//! Как и серверное хранилище, клиент использует одну запись и одно
//! соединение: все операции сериализуются мьютексом и выполняются в
//! `spawn_blocking`, чтобы не блокировать async-runtime. Схема версионируется
//! таблицей `schema_migrations`; миграции применяются последовательно и
//! идемпотентно.
//!
//! Транзакционные границы методов совпадают с требованиями ядра:
//! постановка сообщения в outbox, подтверждение отправки, применение пачки
//! синхронизации вместе с продвижением курсора и т.д.

mod crypto_state;
mod helpers;
mod schema;
mod store_impl;

pub mod secrets;

use std::path::Path;
use std::sync::{Arc, Mutex};

use rchat_core::error::StorageError;
use rusqlite::Connection;

pub use secrets::EncryptedFileSecretStore;

/// Версия схемы, до которой поднимается БД при открытии.
pub const SCHEMA_VERSION: u32 = 3;

/// SQLite-хранилище клиента.
pub struct ClientSqliteStorage {
    conn: Arc<Mutex<Connection>>,
}

impl ClientSqliteStorage {
    /// Открывает (создаёт) файл БД и применяет миграции.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let conn = Connection::open(path.as_ref())
            .map_err(|e| StorageError::Io(format!("sqlite open: {e}")))?;
        Self::prepare(conn)
    }

    /// Открывает БД в памяти (тесты, одноразовые сессии).
    pub fn open_in_memory() -> Result<Self, StorageError> {
        let conn = Connection::open_in_memory()
            .map_err(|e| StorageError::Io(format!("sqlite open: {e}")))?;
        Self::prepare(conn)
    }

    fn prepare(conn: Connection) -> Result<Self, StorageError> {
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| StorageError::Internal(format!("sqlite pragma: {e}")))?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| StorageError::Internal(format!("sqlite pragma: {e}")))?;
        conn.pragma_update(None, "busy_timeout", 5_000)
            .map_err(|e| StorageError::Internal(format!("sqlite pragma: {e}")))?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| StorageError::Internal(format!("sqlite pragma: {e}")))?;
        schema::migrate_to(&conn, SCHEMA_VERSION)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Текущая версия схемы БД (диагностика и миграционные тесты).
    pub fn schema_version(&self) -> Result<u32, StorageError> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| StorageError::Internal("sqlite connection poisoned".into()))?;
        schema::schema_version(&guard)
    }

    /// Диагностика очереди отправки: `(всего незавершённых, готовых к
    /// отправке сейчас)`. Строки outbox существуют только для неотправленных
    /// сообщений, поэтому «всего» — их количество.
    pub async fn outbox_stats(&self, now_ms: u64) -> Result<(u64, u64), StorageError> {
        let now = helpers::to_sql_u64("outbox_stats", now_ms)?;
        self.call(move |conn| {
            let (total, due): (i64, i64) = conn
                .query_row(
                    "SELECT COUNT(*),
                            COALESCE(SUM(CASE WHEN next_attempt_at_ms <= ?1 THEN 1 ELSE 0 END), 0)
                     FROM outbox",
                    rusqlite::params![now],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|e| helpers::map_sql("outbox_stats", e))?;
            Ok((total.max(0) as u64, due.max(0) as u64))
        })
        .await
    }

    /// Выполняет блокирующую работу с БД вне async-runtime.
    async fn call<F, T>(&self, f: F) -> Result<T, StorageError>
    where
        F: FnOnce(&mut Connection) -> Result<T, StorageError> + Send + 'static,
        T: Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let mut guard = conn
                .lock()
                .map_err(|_| StorageError::Internal("sqlite connection poisoned".into()))?;
            f(&mut guard)
        })
        .await
        .map_err(|e| StorageError::Internal(format!("sqlite task failed: {e}")))?
    }
}

//! SQLite-реализация серверного [`Repo`] и сетевой слой бинарника
//! `rchat-server` (TLS + WebSocket, см. [`net`]).
//!
//! Одна запись, одно соединение: все операции сериализуются мьютексом и
//! выполняются в `spawn_blocking`, чтобы не блокировать async-runtime.
//! Транзакционные границы методов совпадают с требованиями сервисного слоя
//! (регистрация, вставка сообщения с событиями, выдача предключа и т.д.).
//!
//! Схема версионируется таблицей `schema_migrations`; миграции применяются
//! последовательно и идемпотентно.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rchat_server::repo::{RepoError, RepoError as SqliteError};
use rusqlite::Connection;

mod convert;
mod helpers;
mod repo_impl;

pub mod net;

/// Версия схемы, до которой поднимается БД при открытии.
pub const SCHEMA_VERSION: u32 = 8;

/// SQLite-репозиторий сервера.
pub struct SqliteRepo {
    conn: Arc<Mutex<Connection>>,
    /// Каталог файлов блобов: содержимое файлов лежит на диске, в БД — только
    /// метаданные (путь, размер, токен, владелец).
    blob_dir: PathBuf,
}

impl SqliteRepo {
    /// Открывает (создаёт) файл БД и применяет миграции.
    ///
    /// Файлы блобов складываются в каталог `blobs` рядом с базой.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RepoError> {
        let path = path.as_ref();
        let conn =
            Connection::open(path).map_err(|e| RepoError::internal(format!("sqlite open: {e}")))?;
        let blob_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("blobs");
        Self::prepare(conn, blob_dir)
    }

    /// Открывает БД в памяти (тесты, локальные запуски без файла).
    pub fn open_in_memory() -> Result<Self, RepoError> {
        let conn = Connection::open_in_memory()
            .map_err(|e| RepoError::internal(format!("sqlite open: {e}")))?;
        let blob_dir = std::env::temp_dir().join(format!(
            "rchat-blobs-{}-{}",
            std::process::id(),
            rchat_proto::now_ms()
        ));
        Self::prepare(conn, blob_dir)
    }

    /// Каталог файлов блобов.
    pub fn blob_dir(&self) -> &Path {
        &self.blob_dir
    }

    /// Путь файла блоба на диске.
    fn blob_path(&self, blob_id: &str) -> PathBuf {
        self.blob_dir.join(format!("{blob_id}.blob"))
    }

    fn prepare(conn: Connection, blob_dir: PathBuf) -> Result<Self, RepoError> {
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| RepoError::internal(format!("sqlite pragma: {e}")))?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| RepoError::internal(format!("sqlite pragma: {e}")))?;
        migrate_to(&conn, SCHEMA_VERSION, &blob_dir)?;
        ensure_blob_dir(&blob_dir)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            blob_dir,
        })
    }

    /// Выполняет блокирующую работу с БД вне async-runtime.
    async fn call<F, T>(&self, f: F) -> Result<T, RepoError>
    where
        F: FnOnce(&mut Connection) -> Result<T, RepoError> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn
                .lock()
                .map_err(|_| RepoError::internal("sqlite connection poisoned"))?;
            f(&mut guard)
        })
        .await
        .map_err(|e| RepoError::internal(format!("sqlite task failed: {e}")))?
    }
}

/// Текущая версия схемы в БД.
pub fn schema_version(conn: &Connection) -> Result<u32, RepoError> {
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !exists {
        return Ok(0);
    }
    let version: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .map_err(|e| RepoError::internal(format!("schema version: {e}")))?;
    Ok(version.max(0) as u32)
}

/// Создаёт каталог файлов блобов (если его ещё нет).
pub(crate) fn ensure_blob_dir(dir: &Path) -> Result<(), RepoError> {
    std::fs::create_dir_all(dir).map_err(|e| RepoError::internal(format!("blob dir: {e}")))
}

/// Применяет миграции до версии `target` (идемпотентно).
///
/// Каждая миграция выполняется в собственной транзакции: сбой оставляет БД
/// на предыдущей версии, повторный запуск продолжает с неё.
pub fn migrate_to(conn: &Connection, target: u32, blob_dir: &Path) -> Result<(), RepoError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at_ms INTEGER NOT NULL
        );",
    )
    .map_err(|e| RepoError::internal(format!("migrations table: {e}")))?;
    let current = schema_version(conn)?;
    for (version, sql) in MIGRATIONS {
        if *version <= current || *version > target {
            continue;
        }
        let tx_result = conn
            .unchecked_transaction()
            .map_err(|e| RepoError::internal(format!("begin migration {version}: {e}")))?;
        tx_result
            .execute_batch(sql)
            .map_err(|e| RepoError::internal(format!("migration {version}: {e}")))?;
        if *version == 6 {
            // Данные блобов переносим из БД на диск: в таблице остаются
            // только метаданные (путь, размер, токен, владелец).
            migrate_blobs_to_disk(&tx_result, blob_dir)
                .map_err(|e| RepoError::internal(format!("migration 6 blobs: {e}")))?;
        }
        tx_result
            .execute(
                "INSERT INTO schema_migrations(version, applied_at_ms) VALUES(?1, ?2)",
                rusqlite::params![version, rchat_proto::now_ms() as i64],
            )
            .map_err(|e| RepoError::internal(format!("record migration {version}: {e}")))?;
        tx_result
            .commit()
            .map_err(|e| RepoError::internal(format!("commit migration {version}: {e}")))?;
    }
    Ok(())
}

/// Переносит накопленные блобы из таблицы `blob_parts` в файлы на диске.
fn migrate_blobs_to_disk(
    tx: &rusqlite::Transaction<'_>,
    blob_dir: &Path,
) -> Result<(), rusqlite::Error> {
    std::fs::create_dir_all(blob_dir).ok();
    let ids: Vec<String> = {
        let mut stmt = tx.prepare("SELECT DISTINCT blob_id FROM blob_parts")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<String>, _>>()?
    };
    for blob_id in ids {
        let parts: Vec<(i64, Vec<u8>)> = {
            let mut stmt = tx.prepare(
                "SELECT offset, data FROM blob_parts WHERE blob_id = ?1 ORDER BY offset",
            )?;
            let rows = stmt.query_map([&blob_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?;
            rows.collect::<Result<Vec<(i64, Vec<u8>)>, _>>()?
        };
        if parts.is_empty() {
            continue;
        }
        let mut buffer = vec![0u8; parts.iter().map(|(_, data)| data.len()).sum()];
        let mut cursor = 0usize;
        for (_, data) in &parts {
            buffer[cursor..cursor + data.len()].copy_from_slice(data);
            cursor += data.len();
        }
        let path = blob_dir.join(format!("{blob_id}.blob"));
        std::fs::write(&path, &buffer).ok();
        tx.execute(
            "UPDATE blobs SET path = ?2, received = ?3, completed = 1 WHERE blob_id = ?1",
            rusqlite::params![
                blob_id,
                path.to_string_lossy().to_string(),
                buffer.len() as i64
            ],
        )?;
        tx.execute("DELETE FROM blob_parts WHERE blob_id = ?1", [&blob_id])?;
    }
    Ok(())
}

/// Миграции по возрастанию версии. SQL не изменяется задним числом.
pub const MIGRATIONS: &[(u32, &str)] = &[
    (
        1,
        r#"
        CREATE TABLE users (
            user_id TEXT NOT NULL PRIMARY KEY,
            username TEXT NOT NULL UNIQUE,
            display_name TEXT NOT NULL,
            password_hash TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            disabled INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE devices (
            device_id TEXT NOT NULL PRIMARY KEY,
            user_id TEXT NOT NULL REFERENCES users(user_id),
            name TEXT NOT NULL,
            platform TEXT NOT NULL,
            e2e_version INTEGER NOT NULL,
            ed25519 BLOB NOT NULL DEFAULT X'',
            curve25519 BLOB NOT NULL DEFAULT X'',
            created_at_ms INTEGER NOT NULL,
            last_seen_at_ms INTEGER NOT NULL,
            revoked_at_ms INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX devices_user_idx ON devices(user_id);

        CREATE TABLE prekeys (
            device_id TEXT NOT NULL REFERENCES devices(device_id),
            key_id INTEGER NOT NULL,
            public_key BLOB NOT NULL,
            fallback INTEGER NOT NULL DEFAULT 0,
            created_at_ms INTEGER NOT NULL,
            consumed_at_ms INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (device_id, key_id)
        );

        CREATE TABLE sessions (
            session_id TEXT NOT NULL PRIMARY KEY,
            token_hash BLOB NOT NULL UNIQUE,
            user_id TEXT NOT NULL,
            device_id TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL,
            expires_at_ms INTEGER NOT NULL,
            last_used_at_ms INTEGER NOT NULL,
            revoked_at_ms INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX sessions_device_idx ON sessions(device_id);

        CREATE TABLE chats (
            chat_id TEXT NOT NULL PRIMARY KEY,
            kind INTEGER NOT NULL,
            e2e_mode INTEGER NOT NULL,
            pair_key TEXT UNIQUE,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            last_server_id INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE chat_members (
            chat_id TEXT NOT NULL REFERENCES chats(chat_id),
            user_id TEXT NOT NULL,
            joined_at_ms INTEGER NOT NULL,
            PRIMARY KEY (chat_id, user_id)
        );
        CREATE INDEX chat_members_user_idx ON chat_members(user_id);

        CREATE TABLE messages (
            server_id INTEGER NOT NULL PRIMARY KEY,
            chat_id TEXT NOT NULL REFERENCES chats(chat_id),
            sender_user_id TEXT NOT NULL,
            sender_device_id TEXT NOT NULL,
            client_message_id TEXT NOT NULL,
            content_kind TEXT NOT NULL,
            reply_to_server_id INTEGER NOT NULL DEFAULT 0,
            created_at_ms INTEGER NOT NULL,
            edited_at_ms INTEGER NOT NULL DEFAULT 0,
            deleted_at_ms INTEGER NOT NULL DEFAULT 0,
            sender_curve25519 BLOB NOT NULL DEFAULT X'',
            olm_type INTEGER NOT NULL DEFAULT 0,
            plaintext_dev TEXT
        );
        CREATE UNIQUE INDEX messages_idem_idx
            ON messages(chat_id, sender_device_id, client_message_id);
        CREATE INDEX messages_chat_idx ON messages(chat_id, server_id DESC);

        CREATE TABLE message_recipients (
            server_id INTEGER NOT NULL REFERENCES messages(server_id),
            device_id TEXT NOT NULL,
            ciphertext BLOB NOT NULL,
            e2e_version INTEGER NOT NULL,
            olm_type INTEGER NOT NULL,
            sender_curve25519 BLOB NOT NULL DEFAULT X'',
            PRIMARY KEY (server_id, device_id)
        );

        CREATE TABLE sync_events (
            device_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            kind_hint INTEGER NOT NULL,
            payload BLOB NOT NULL,
            created_at_ms INTEGER NOT NULL,
            PRIMARY KEY (device_id, seq)
        );

        CREATE TABLE device_sync_state (
            device_id TEXT NOT NULL PRIMARY KEY,
            cursor INTEGER NOT NULL DEFAULT 0,
            next_seq INTEGER NOT NULL DEFAULT 1,
            updated_at_ms INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE receipts (
            chat_id TEXT NOT NULL,
            user_id TEXT NOT NULL,
            up_to_server_id INTEGER NOT NULL,
            read INTEGER NOT NULL DEFAULT 0,
            updated_at_ms INTEGER NOT NULL,
            PRIMARY KEY (chat_id, user_id)
        );
        "#,
    ),
    (
        2,
        r#"
        -- Индексы под выборки синхронизации и очистку журнала.
        CREATE INDEX sync_events_created_idx ON sync_events(created_at_ms);
        CREATE INDEX sync_events_device_seq_idx ON sync_events(device_id, seq);
        "#,
    ),
    (
        3,
        r#"
        -- Групповые чаты (protocol minor 1): название и роли участников,
        -- megolm-ciphertext сообщений и вид получательской копии.
        ALTER TABLE chats ADD COLUMN name TEXT NOT NULL DEFAULT '';
        ALTER TABLE chat_members ADD COLUMN role INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE messages ADD COLUMN group_ciphertext BLOB;
        ALTER TABLE messages ADD COLUMN group_session_id TEXT NOT NULL DEFAULT '';
        ALTER TABLE message_recipients ADD COLUMN payload_kind INTEGER NOT NULL DEFAULT 0;
        "#,
    ),
    (
        4,
        r#"
    -- Файлы (блобы). Сервер хранит только шифротекст: ключ шифрования
    -- файла едет внутри сообщения, поэтому содержимое серверу неизвестно.
    CREATE TABLE blobs (
        blob_id TEXT NOT NULL PRIMARY KEY,
        token TEXT NOT NULL DEFAULT '',
        chat_id TEXT NOT NULL,
        owner_user_id TEXT NOT NULL,
        total_size INTEGER NOT NULL,
        received INTEGER NOT NULL DEFAULT 0,
        completed INTEGER NOT NULL DEFAULT 0,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL
    );
    CREATE INDEX blobs_token_idx ON blobs(token);
    CREATE TABLE blob_parts (
        blob_id TEXT NOT NULL,
        offset INTEGER NOT NULL,
        data BLOB NOT NULL,
        PRIMARY KEY (blob_id, offset)
    );
    "#,
    ),
    (
        5,
        r#"
    -- Альбомы (медиагруппы): сообщения одного альбома делят grouped_id.
    ALTER TABLE messages ADD COLUMN grouped_id TEXT NOT NULL DEFAULT '';
    "#,
    ),
    (
        7,
        r#"
    -- Облачный режим: ключ чата (сервер знает его и может расшифровать
    -- содержимое) и облачные шифротексты сообщений.
    ALTER TABLE chats ADD COLUMN cloud_key BLOB;
    ALTER TABLE messages ADD COLUMN cloud_ciphertext BLOB;
    ALTER TABLE messages ADD COLUMN cloud_nonce BLOB;
    "#,
    ),
    (
        8,
        r#"
    -- Облачный режим файлов: ключ аккаунта (сервер знает его и может
    -- расшифровать файл), отпечаток содержимого для глобальной
    -- дедупликации и метаданные файла (имя, вид, MIME, размер).
    ALTER TABLE users ADD COLUMN cloud_account_key BLOB;
    ALTER TABLE blobs ADD COLUMN cloud INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE blobs ADD COLUMN content_hash BLOB;
    ALTER TABLE blobs ADD COLUMN name TEXT NOT NULL DEFAULT '';
    ALTER TABLE blobs ADD COLUMN kind TEXT NOT NULL DEFAULT '';
    ALTER TABLE blobs ADD COLUMN mime TEXT NOT NULL DEFAULT '';
    ALTER TABLE blobs ADD COLUMN plaintext_size INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE blobs ADD COLUMN nonce BLOB;
    "#,
    ),
    (
        6,
        r#"
    -- Содержимое блобов переносится на диск: в БД остаются метаданные.
    ALTER TABLE blobs ADD COLUMN path TEXT NOT NULL DEFAULT '';
    "#,
    ),
];

/// Назначает следующий seq для устройства (внутри уже открытой транзакции).
pub(crate) fn alloc_seq(conn: &Connection, device_id: &str) -> Result<u64, RepoError> {
    conn.execute(
        "INSERT OR IGNORE INTO device_sync_state(device_id, cursor, next_seq, updated_at_ms)
         VALUES(?1, 0, 1, 0)",
        rusqlite::params![device_id],
    )
    .map_err(|e| RepoError::internal(format!("sync state: {e}")))?;
    let seq: i64 = conn
        .query_row(
            "SELECT next_seq FROM device_sync_state WHERE device_id = ?1",
            rusqlite::params![device_id],
            |row| row.get(0),
        )
        .map_err(|e| RepoError::internal(format!("sync state read: {e}")))?;
    conn.execute(
        "UPDATE device_sync_state SET next_seq = next_seq + 1 WHERE device_id = ?1",
        rusqlite::params![device_id],
    )
    .map_err(|e| RepoError::internal(format!("sync state update: {e}")))?;
    Ok(seq.max(1) as u64)
}

/// Ошибка SQLite → доменная ошибка. Уникальные ограничения распознаются
/// по имени индекса, чтобы сервис мог вернуть точный код.
pub(crate) fn map_sql(context: &str, error: rusqlite::Error) -> SqliteError {
    if let rusqlite::Error::SqliteFailure(err, Some(message)) = &error {
        if err.code == rusqlite::ErrorCode::ConstraintViolation {
            if message.contains("users.username") {
                return SqliteError::Conflict("username_taken".into());
            }
            if message.contains("chats.pair_key") {
                return SqliteError::Conflict("chat_pair".into());
            }
            if message.contains("messages_idem_idx") {
                return SqliteError::Conflict("duplicate_message".into());
            }
            return SqliteError::Conflict(format!("{context}: {message}"));
        }
    }
    SqliteError::internal(format!("{context}: {error}"))
}

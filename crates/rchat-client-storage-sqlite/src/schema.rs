//! Миграции клиентской схемы.
//!
//! SQL миграций не изменяется задним числом: новые изменения — только новой
//! версией в конце списка. Каждая миграция применяется в отдельной
//! транзакции; сбой оставляет БД на предыдущей версии, повторный запуск
//! продолжает с неё.

use rusqlite::Connection;

use rchat_core::error::StorageError;

/// Применяет миграции до версии `target` (идемпотентно).
pub(crate) fn migrate_to(conn: &Connection, target: u32) -> Result<(), StorageError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at_ms INTEGER NOT NULL
        );",
    )
    .map_err(|e| StorageError::Internal(format!("migrations table: {e}")))?;
    let current = schema_version(conn)?;
    for (version, sql) in MIGRATIONS {
        if *version <= current || *version > target {
            continue;
        }
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| StorageError::Transaction(format!("begin migration {version}: {e}")))?;
        tx.execute_batch(sql)
            .map_err(|e| StorageError::Transaction(format!("migration {version}: {e}")))?;
        tx.execute(
            "INSERT INTO schema_migrations(version, applied_at_ms) VALUES(?1, ?2)",
            rusqlite::params![version, rchat_proto::now_ms() as i64],
        )
        .map_err(|e| StorageError::Transaction(format!("record migration {version}: {e}")))?;
        tx.commit()
            .map_err(|e| StorageError::Transaction(format!("commit migration {version}: {e}")))?;
    }
    Ok(())
}

/// Текущая версия схемы в БД (0 — таблицы миграций ещё нет).
pub(crate) fn schema_version(conn: &Connection) -> Result<u32, StorageError> {
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
        .map_err(|e| StorageError::Internal(format!("schema version: {e}")))?;
    Ok(version.max(0) as u32)
}

/// Миграции по возрастанию версии.
pub(crate) const MIGRATIONS: &[(u32, &str)] = &[
    (
        1,
        r#"
    -- Профиль этого устройства (одна строка).
    CREATE TABLE account (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        user_id TEXT NOT NULL,
        username TEXT NOT NULL,
        display_name TEXT NOT NULL,
        device_id TEXT NOT NULL
    );

    -- Кеш пользователей и устройств (только публичные данные).
    CREATE TABLE users (
        user_id TEXT NOT NULL PRIMARY KEY,
        username TEXT NOT NULL,
        display_name TEXT NOT NULL
    );

    CREATE TABLE devices (
        device_id TEXT NOT NULL PRIMARY KEY,
        user_id TEXT NOT NULL,
        name TEXT NOT NULL,
        platform TEXT NOT NULL,
        e2e_version INTEGER NOT NULL,
        ed25519 BLOB NOT NULL DEFAULT X'',
        curve25519 BLOB NOT NULL DEFAULT X'',
        revoked_at_ms INTEGER
    );
    CREATE INDEX devices_user_idx ON devices(user_id);

    CREATE TABLE chats (
        chat_id TEXT NOT NULL PRIMARY KEY,
        kind INTEGER NOT NULL,
        e2e_mode INTEGER NOT NULL,
        title TEXT NOT NULL,
        peer_user_id TEXT,
        created_at_ms INTEGER NOT NULL,
        updated_at_ms INTEGER NOT NULL,
        last_server_id INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX chats_updated_idx ON chats(updated_at_ms DESC);

    CREATE TABLE chat_members (
        chat_id TEXT NOT NULL,
        user_id TEXT NOT NULL,
        username TEXT NOT NULL,
        display_name TEXT NOT NULL,
        joined_at_ms INTEGER NOT NULL,
        PRIMARY KEY (chat_id, user_id)
    );
    CREATE INDEX chat_members_user_idx ON chat_members(user_id);

    -- Локальные сообщения: свои (с локальным local_id) и принятые
    -- (local_id = 's:<server_id>'). server_id уникален там, где известен.
    CREATE TABLE messages (
        local_id TEXT NOT NULL PRIMARY KEY,
        server_id INTEGER UNIQUE,
        chat_id TEXT NOT NULL,
        sender_user_id TEXT NOT NULL,
        sender_device_id TEXT NOT NULL,
        content_kind TEXT NOT NULL,
        body TEXT,
        reply_to_server_id INTEGER,
        status INTEGER,
        created_at_ms INTEGER NOT NULL,
        edited_at_ms INTEGER,
        deleted_at_ms INTEGER
    );
    CREATE INDEX messages_chat_idx ON messages(chat_id, server_id DESC, created_at_ms DESC);

    -- Очередь отправки. request — сериализованный SendMessageRequest.
    CREATE TABLE outbox (
        local_id TEXT NOT NULL PRIMARY KEY,
        chat_id TEXT NOT NULL,
        idempotency_key TEXT NOT NULL,
        request BLOB NOT NULL,
        state INTEGER NOT NULL,
        attempts INTEGER NOT NULL DEFAULT 0,
        next_attempt_at_ms INTEGER NOT NULL DEFAULT 0,
        last_error_code INTEGER,
        last_error TEXT,
        created_at_ms INTEGER NOT NULL,
        server_message_id INTEGER
    );
    CREATE INDEX outbox_due_idx ON outbox(state, next_attempt_at_ms, created_at_ms);

    CREATE TABLE sync_state (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        cursor INTEGER NOT NULL DEFAULT 0
    );
    INSERT OR IGNORE INTO sync_state(id, cursor) VALUES (1, 0);

    CREATE TABLE receipts (
        chat_id TEXT NOT NULL,
        user_id TEXT NOT NULL,
        delivered_up_to INTEGER NOT NULL DEFAULT 0,
        read_up_to INTEGER NOT NULL DEFAULT 0,
        updated_at_ms INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (chat_id, user_id)
    );

    -- Состояние E2E-провайдера (olm-аккаунт и сессии).
    CREATE TABLE crypto_state (
        key TEXT NOT NULL PRIMARY KEY,
        value BLOB NOT NULL
    );
    "#,
    ),
    (
        2,
        r#"
    -- Групповые чаты: роль участника (0 — direct/неизвестно, 1 — member,
    -- 2 — owner) и учёт выданных ключей megolm-сессий, чтобы не рассылать
    -- ключ повторно и досылать его только новым устройствам.
    ALTER TABLE chat_members ADD COLUMN role INTEGER NOT NULL DEFAULT 0;

    CREATE TABLE group_key_shares (
        chat_id TEXT NOT NULL,
        session_id TEXT NOT NULL,
        device_id TEXT NOT NULL,
        shared_at_ms INTEGER NOT NULL,
        PRIMARY KEY (chat_id, session_id, device_id)
    );
    "#,
    ),
    (
        5,
        r#"
    -- Постер-кадр видео (второй блоб) и пики звуковой волны голосового.
    ALTER TABLE messages ADD COLUMN attachment_thumb_ref TEXT;
    ALTER TABLE messages ADD COLUMN attachment_thumb_key BLOB;
    ALTER TABLE messages ADD COLUMN attachment_thumb_nonce BLOB;
    ALTER TABLE messages ADD COLUMN attachment_thumb_size INTEGER;
    ALTER TABLE messages ADD COLUMN attachment_thumb_mime TEXT;
    ALTER TABLE messages ADD COLUMN attachment_waveform BLOB;

    -- Локальный кэш загрузок: хэш содержимого → уже залитый блоб и ключ.
    -- Позволяет не заливать один и тот же файл повторно, не раскрывая ключ
    -- серверу (сервер видит только шифротекст).
    CREATE TABLE uploaded_blobs (
        hash BLOB NOT NULL PRIMARY KEY,
        blob_ref TEXT NOT NULL,
        key BLOB NOT NULL,
        nonce BLOB NOT NULL,
        size INTEGER NOT NULL,
        created_at_ms INTEGER NOT NULL
    );
    "#,
    ),
    (
        4,
        r#"
    -- Файлы едут отдельным блобом: в сообщении лежат ссылка на блоб и ключ
    -- шифрования, а содержимое (attachment_data) — локальный кэш.
    ALTER TABLE messages ADD COLUMN attachment_blob_ref TEXT;
    ALTER TABLE messages ADD COLUMN attachment_key BLOB;
    ALTER TABLE messages ADD COLUMN attachment_nonce BLOB;
    "#,
    ),
    (
        3,
        r#"
    -- Вложения сообщений (файл/фото/видео/голос): байты лежат локально,
    -- метаданные — рядом. Само вложение едет внутри зашифрованного payload,
    -- поэтому сервер его не видит.
    ALTER TABLE messages ADD COLUMN attachment_kind TEXT;
    ALTER TABLE messages ADD COLUMN attachment_name TEXT;
    ALTER TABLE messages ADD COLUMN attachment_mime TEXT;
    ALTER TABLE messages ADD COLUMN attachment_size INTEGER;
    ALTER TABLE messages ADD COLUMN attachment_data BLOB;
    "#,
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// База, созданная до появления ролей и учёта выдач ключей (v1),
    /// поднимается до текущей версии без потери данных: прежним участникам
    /// достаётся роль 0, новые столбцы и таблицы создаются.
    #[test]
    fn upgrade_from_v1_preserves_data() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("client.sqlite3");
        let conn = Connection::open(&path).expect("open");
        migrate_to(&conn, 1).expect("v1");
        conn.execute_batch(
            "INSERT INTO chats(chat_id, kind, e2e_mode, title, peer_user_id,
                 created_at_ms, updated_at_ms, last_server_id)
             VALUES('c1', 1, 1, 'Bob', 'u_bob', 1, 1, 0);
             INSERT INTO chat_members(chat_id, user_id, username, display_name, joined_at_ms)
             VALUES('c1', 'u_bob', 'bob', 'Bob', 1);",
        )
        .expect("seed v1");
        assert_eq!(schema_version(&conn).expect("version"), 1);

        migrate_to(&conn, crate::SCHEMA_VERSION).expect("upgrade");
        assert_eq!(
            schema_version(&conn).expect("version"),
            crate::SCHEMA_VERSION
        );
        let role: i64 = conn
            .query_row(
                "SELECT role FROM chat_members WHERE user_id = 'u_bob'",
                [],
                |row| row.get(0),
            )
            .expect("role");
        assert_eq!(role, 0, "прежним участникам достаётся роль 0");
        let title: String = conn
            .query_row("SELECT title FROM chats", [], |row| row.get(0))
            .expect("title");
        assert_eq!(title, "Bob", "данные v1 сохранены");
        let shares: i64 = conn
            .query_row("SELECT COUNT(*) FROM group_key_shares", [], |row| {
                row.get(0)
            })
            .expect("group_key_shares");
        assert_eq!(shares, 0);
    }

    /// Повторное применение миграций ничего не меняет.
    #[test]
    fn migration_is_idempotent() {
        let conn = Connection::open_in_memory().expect("open");
        migrate_to(&conn, crate::SCHEMA_VERSION).expect("first");
        migrate_to(&conn, crate::SCHEMA_VERSION).expect("second");
        assert_eq!(
            schema_version(&conn).expect("version"),
            crate::SCHEMA_VERSION
        );
    }
}

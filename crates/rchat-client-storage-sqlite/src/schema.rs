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
];

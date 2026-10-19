//! Вспомогательные функции SQLite-репозитория (вне trait-impl).
#![allow(dead_code)]

use std::collections::HashMap;

use rchat_server::model::{
    ChatRecord, DeviceRecord, MemberRecord, MessageRecord, NewSyncEvent, PreKeyRecord,
    RecipientRecord, UserRecord,
};
use rchat_server::repo::RepoError;
use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::convert::*;
use crate::{alloc_seq, map_sql};

pub(crate) const USER_COLUMNS: &str =
    "user_id, username, display_name, password_hash, created_at_ms, updated_at_ms, disabled";
pub(crate) const DEVICE_COLUMNS: &str =
    "device_id, user_id, name, platform, e2e_version, ed25519, \
     curve25519, created_at_ms, last_seen_at_ms, revoked_at_ms";
pub(crate) const SESSION_COLUMNS: &str =
    "session_id, token_hash, user_id, device_id, created_at_ms, \
     expires_at_ms, last_used_at_ms, revoked_at_ms";

pub(crate) fn random_id(context: &str) -> Result<String, RepoError> {
    rchat_proto::random_id_hex(16).map_err(|e| RepoError::internal(format!("{context}: {e}")))
}

pub(crate) fn find_user(conn: &Connection, user_id: &str) -> Result<Option<UserRecord>, RepoError> {
    let sql = format!("SELECT {USER_COLUMNS} FROM users WHERE user_id = ?1");
    conn.query_row(&sql, params![user_id], user_from_row)
        .optional()
        .map_err(|e| RepoError::internal(format!("find_user: {e}")))
}

pub(crate) fn find_device(
    conn: &Connection,
    user_id: &str,
    device_id: &str,
) -> Result<Option<DeviceRecord>, RepoError> {
    let sql = format!("SELECT {DEVICE_COLUMNS} FROM devices WHERE user_id = ?1 AND device_id = ?2");
    conn.query_row(&sql, params![user_id, device_id], device_from_row)
        .optional()
        .map_err(|e| RepoError::internal(format!("find_device: {e}")))
}

pub(crate) fn find_device_by_id(
    conn: &Connection,
    device_id: &str,
) -> Result<Option<DeviceRecord>, RepoError> {
    let sql = format!("SELECT {DEVICE_COLUMNS} FROM devices WHERE device_id = ?1");
    conn.query_row(&sql, params![device_id], device_from_row)
        .optional()
        .map_err(|e| RepoError::internal(format!("find_device_by_id: {e}")))
}

pub(crate) fn list_devices(
    conn: &Connection,
    user_id: &str,
    include_revoked: bool,
) -> Result<Vec<DeviceRecord>, RepoError> {
    let sql = if include_revoked {
        format!("SELECT {DEVICE_COLUMNS} FROM devices WHERE user_id = ?1 ORDER BY created_at_ms, device_id")
    } else {
        format!("SELECT {DEVICE_COLUMNS} FROM devices WHERE user_id = ?1 AND revoked_at_ms = 0 ORDER BY created_at_ms, device_id")
    };
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| RepoError::internal(format!("prepare list_devices: {e}")))?;
    let rows = stmt
        .query_map(params![user_id], device_from_row)
        .map_err(|e| RepoError::internal(format!("query list_devices: {e}")))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| RepoError::internal(format!("rows list_devices: {e}")))
}

pub(crate) fn devices_for_users(
    conn: &Connection,
    user_ids: &[String],
    include_revoked: bool,
) -> Result<Vec<DeviceRecord>, RepoError> {
    if user_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders: Vec<String> = (1..=user_ids.len()).map(|i| format!("?{i}")).collect();
    let filter = if include_revoked {
        String::new()
    } else {
        " AND revoked_at_ms = 0".to_string()
    };
    let sql = format!(
        "SELECT {DEVICE_COLUMNS} FROM devices WHERE user_id IN ({}){} ORDER BY created_at_ms, device_id",
        placeholders.join(", "),
        filter
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| RepoError::internal(format!("prepare devices_for_users: {e}")))?;
    let params: Vec<&dyn rusqlite::ToSql> = user_ids
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();
    let rows = stmt
        .query_map(params.as_slice(), device_from_row)
        .map_err(|e| RepoError::internal(format!("query devices_for_users: {e}")))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| RepoError::internal(format!("rows devices_for_users: {e}")))
}

/// Вставка предключей в открытой транзакции. Возвращает число новых
/// одноразовых ключей.
pub(crate) fn insert_prekeys(
    tx: &Transaction<'_>,
    device_id: &str,
    keys: &[PreKeyRecord],
    fallback: Option<&PreKeyRecord>,
) -> Result<u32, RepoError> {
    let mut inserted = 0u32;
    for key in keys {
        let changed = tx
            .execute(
                "INSERT OR IGNORE INTO prekeys(device_id, key_id, public_key, fallback, created_at_ms, consumed_at_ms)
                 VALUES(?1, ?2, ?3, 0, ?4, 0)",
                params![
                    device_id,
                    key.key_id as i64,
                    key.public_key,
                    key.created_at_ms as i64
                ],
            )
            .map_err(|e| map_sql("insert prekey", e))?;
        inserted += changed as u32;
    }
    if let Some(fb) = fallback {
        tx.execute(
            "DELETE FROM prekeys WHERE device_id = ?1 AND fallback = 1",
            params![device_id],
        )
        .map_err(|e| RepoError::internal(format!("replace fallback: {e}")))?;
        tx.execute(
            "INSERT INTO prekeys(device_id, key_id, public_key, fallback, created_at_ms, consumed_at_ms)
             VALUES(?1, ?2, ?3, 1, ?4, 0)",
            params![
                device_id,
                fb.key_id as i64,
                fb.public_key,
                fb.created_at_ms as i64
            ],
        )
        .map_err(|e| map_sql("insert fallback", e))?;
    }
    Ok(inserted)
}

pub(crate) const CHAT_COLUMNS: &str =
    "chat_id, kind, e2e_mode, name, cloud_key, created_at_ms, updated_at_ms, last_server_id";
pub(crate) const MEMBER_COLUMNS: &str = "chat_id, user_id, joined_at_ms, role";
pub(crate) const MESSAGE_COLUMNS: &str = "server_id, chat_id, sender_user_id, sender_device_id, \
     client_message_id, content_kind, reply_to_server_id, created_at_ms, edited_at_ms, \
     deleted_at_ms, sender_curve25519, olm_type, plaintext_dev, group_ciphertext, group_session_id, \
     grouped_id, cloud_ciphertext, cloud_nonce";
pub(crate) const RECIPIENT_COLUMNS: &str =
    "server_id, device_id, ciphertext, e2e_version, olm_type, sender_curve25519, payload_kind";

/// Ключ уникальности личного чата (порядок участников не важен).
pub(crate) fn pair_key(user_a: &str, user_b: &str) -> String {
    if user_a <= user_b {
        format!("{user_a}|{user_b}")
    } else {
        format!("{user_b}|{user_a}")
    }
}

pub(crate) fn members_of(conn: &Connection, chat_id: &str) -> Result<Vec<MemberRecord>, RepoError> {
    let sql = format!(
        "SELECT {MEMBER_COLUMNS} FROM chat_members WHERE chat_id = ?1 ORDER BY joined_at_ms, user_id"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| RepoError::internal(format!("prepare members: {e}")))?;
    let rows = stmt
        .query_map(params![chat_id], member_from_row)
        .map_err(|e| RepoError::internal(format!("query members: {e}")))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| RepoError::internal(format!("rows members: {e}")))
}

pub(crate) fn get_chat(
    conn: &Connection,
    chat_id: &str,
) -> Result<Option<(ChatRecord, Vec<MemberRecord>)>, RepoError> {
    let sql = format!("SELECT {CHAT_COLUMNS} FROM chats WHERE chat_id = ?1");
    let chat: Option<ChatRecord> = conn
        .query_row(&sql, params![chat_id], chat_from_row)
        .optional()
        .map_err(|e| RepoError::internal(format!("get_chat: {e}")))?;
    match chat {
        Some(chat) => {
            let members = members_of(conn, chat_id)?;
            Ok(Some((chat, members)))
        }
        None => Ok(None),
    }
}

pub(crate) fn get_message(
    conn: &Connection,
    server_id: u64,
) -> Result<Option<MessageRecord>, RepoError> {
    let sql = format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE server_id = ?1");
    conn.query_row(&sql, params![server_id as i64], message_from_row)
        .optional()
        .map_err(|e| RepoError::internal(format!("get_message: {e}")))
}

pub(crate) fn insert_events(
    conn: &Connection,
    events: &[NewSyncEvent],
) -> Result<Vec<(String, u64)>, RepoError> {
    let mut seqs = Vec::with_capacity(events.len());
    for event in events {
        let seq = alloc_seq(conn, &event.device_id)?;
        conn.execute(
            "INSERT INTO sync_events(device_id, seq, kind_hint, payload, created_at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![
                event.device_id,
                seq as i64,
                event.kind_hint as i64,
                event.payload,
                event.created_at_ms as i64
            ],
        )
        .map_err(|e| map_sql("insert sync event", e))?;
        seqs.push((event.device_id.clone(), seq));
    }
    Ok(seqs)
}

pub(crate) fn find_by_idempotency(
    conn: &Connection,
    chat_id: &str,
    sender_device_id: &str,
    client_message_id: &str,
) -> Result<Option<MessageRecord>, RepoError> {
    let sql = format!(
        "SELECT {MESSAGE_COLUMNS} FROM messages
         WHERE chat_id = ?1 AND sender_device_id = ?2 AND client_message_id = ?3"
    );
    conn.query_row(
        &sql,
        params![chat_id, sender_device_id, client_message_id],
        message_from_row,
    )
    .optional()
    .map_err(|e| RepoError::internal(format!("find idempotency: {e}")))
}

pub(crate) fn insert_message_row(
    conn: &Connection,
    message: &MessageRecord,
) -> Result<(), RepoError> {
    conn.execute(
        "INSERT INTO messages(server_id, chat_id, sender_user_id, sender_device_id,
             client_message_id, content_kind, reply_to_server_id, created_at_ms,
             edited_at_ms, deleted_at_ms, sender_curve25519, olm_type, plaintext_dev,
             group_ciphertext, group_session_id, grouped_id, cloud_ciphertext, cloud_nonce)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
        params![
            message.server_id as i64,
            message.chat_id,
            message.sender_user_id,
            message.sender_device_id,
            message.client_message_id,
            message.content_kind,
            message.reply_to_server_id as i64,
            message.created_at_ms as i64,
            message.edited_at_ms as i64,
            message.deleted_at_ms as i64,
            message.sender_curve25519,
            message.olm_type as i64,
            message.plaintext_dev,
            message.group_ciphertext,
            message.group_session_id,
            message.grouped_id,
            message.cloud_ciphertext,
            message.cloud_nonce
        ],
    )
    .map_err(|e| map_sql("insert message", e))?;
    Ok(())
}

pub(crate) fn insert_recipient(
    conn: &Connection,
    recipient: &RecipientRecord,
) -> Result<(), RepoError> {
    conn.execute(
        "INSERT OR REPLACE INTO message_recipients(server_id, device_id, ciphertext,
             e2e_version, olm_type, sender_curve25519, payload_kind)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            recipient.server_id as i64,
            recipient.device_id,
            recipient.ciphertext,
            recipient.e2e_version as i64,
            recipient.olm_type as i64,
            recipient.sender_curve25519,
            recipient.payload_kind as i64
        ],
    )
    .map_err(|e| map_sql("insert recipient", e))?;
    Ok(())
}

/// Обновляет `updated_at_ms` чата (внутри уже открытой транзакции).
pub(crate) fn bump_chat_updated_at(
    conn: &Connection,
    chat_id: &str,
    now_ms: u64,
) -> Result<(), RepoError> {
    conn.execute(
        "UPDATE chats SET updated_at_ms = MAX(updated_at_ms, ?2) WHERE chat_id = ?1",
        params![chat_id, now_ms as i64],
    )
    .map_err(|e| RepoError::internal(format!("bump chat updated_at: {e}")))?;
    Ok(())
}

/// Заготовка на будущее: список пользователей чатов (не используется сервисом).
#[allow(dead_code)]
pub(crate) fn members_lookup(
    conn: &Connection,
    user_id: &str,
) -> Result<HashMap<String, Vec<MemberRecord>>, RepoError> {
    let mut stmt = conn
        .prepare(
            "SELECT m.chat_id, m.user_id, m.joined_at_ms FROM chat_members m
             JOIN chat_members me ON me.chat_id = m.chat_id
             WHERE me.user_id = ?1",
        )
        .map_err(|e| RepoError::internal(format!("prepare members_lookup: {e}")))?;
    let rows = stmt
        .query_map(params![user_id], member_from_row)
        .map_err(|e| RepoError::internal(format!("query members_lookup: {e}")))?;
    let mut map: HashMap<String, Vec<MemberRecord>> = HashMap::new();
    for row in rows {
        let member = row.map_err(|e| RepoError::internal(format!("row: {e}")))?;
        map.entry(member.chat_id.clone()).or_default().push(member);
    }
    Ok(map)
}

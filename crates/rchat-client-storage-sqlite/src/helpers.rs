//! Преобразования значений SQLite ↔ доменные типы и общие фрагменты SQL.

use rchat_core::error::StorageError;
use rchat_core::payload::{Attachment, AttachmentKind, BlobRef};
use rchat_core::types::{
    ChatRecord, DeviceRecord, MemberRecord, MessageRecord, MessageStatus, OutboxRecord,
    ReceiptRecord, UserRecord,
};
use rchat_proto::proto;

/// `u64` → `INTEGER` SQLite. Значения протокола (server_id, время, seq)
/// всегда меньше `i64::MAX`; нарушение — внутренняя ошибка, а не тихая
/// потеря точности.
pub(crate) fn to_sql_u64(context: &str, value: u64) -> Result<i64, StorageError> {
    i64::try_from(value).map_err(|_| {
        StorageError::Internal(format!(
            "{context}: значение {value} не помещается в SQLite INTEGER"
        ))
    })
}

/// `INTEGER` SQLite → `u64` (отрицательные значения невозможны в схеме;
/// трактуются как 0, чтобы не паниковать на повреждённых данных).
pub(crate) fn from_sql_u64(value: i64) -> u64 {
    value.max(0) as u64
}

/// Необязательное `u64` → `INTEGER`.
pub(crate) fn opt_to_sql_u64(value: Option<u64>) -> Option<i64> {
    value.map(|v| v.min(i64::MAX as u64) as i64)
}

pub(crate) fn chat_kind_to_sql(kind: proto::ChatKind) -> i64 {
    kind as i32 as i64
}

pub(crate) fn e2e_mode_to_sql(mode: proto::E2eMode) -> i64 {
    mode as i32 as i64
}

pub(crate) fn role_to_sql(role: proto::ChatMemberRole) -> i64 {
    role as i32 as i64
}

/// Строка `chats` → доменная запись. Позиции колонок фиксированы
/// константой [`CHAT_COLUMNS`].
pub(crate) fn chat_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChatRecord> {
    let kind: i64 = row.get(1)?;
    let e2e_mode: i64 = row.get(2)?;
    Ok(ChatRecord {
        chat_id: row.get(0)?,
        kind: proto::ChatKind::try_from(kind as i32).unwrap_or(proto::ChatKind::Unspecified),
        e2e_mode: proto::E2eMode::try_from(e2e_mode as i32).unwrap_or(proto::E2eMode::Unspecified),
        title: row.get(3)?,
        peer_user_id: row.get(4)?,
        created_at_ms: from_sql_u64(row.get(5)?),
        updated_at_ms: from_sql_u64(row.get(6)?),
        last_server_id: from_sql_u64(row.get(7)?),
    })
}

pub(crate) const CHAT_COLUMNS: &str =
    "chat_id, kind, e2e_mode, title, peer_user_id, created_at_ms, updated_at_ms, last_server_id";

/// Строка `messages` → доменная запись. Позиции колонок фиксированы
/// константой [`MESSAGE_COLUMNS`].
pub(crate) fn message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageRecord> {
    let server_id: Option<i64> = row.get(1)?;
    let status: Option<i64> = row.get(8)?;
    let reply_to: Option<i64> = row.get(7)?;
    let edited: Option<i64> = row.get(10)?;
    let deleted: Option<i64> = row.get(11)?;
    let attachment_kind: Option<String> = row.get(12)?;
    let attachment_name: Option<String> = row.get(13)?;
    let attachment_mime: Option<String> = row.get(14)?;
    let attachment_size: Option<i64> = row.get(15)?;
    let attachment_data: Option<Vec<u8>> = row.get(16)?;
    let attachment_blob_ref: Option<String> = row.get(17)?;
    let attachment_key: Option<Vec<u8>> = row.get(18)?;
    let attachment_nonce: Option<Vec<u8>> = row.get(19)?;
    let attachment_thumb_ref: Option<String> = row.get(20)?;
    let attachment_thumb_key: Option<Vec<u8>> = row.get(21)?;
    let attachment_thumb_nonce: Option<Vec<u8>> = row.get(22)?;
    let attachment_thumb_size: Option<i64> = row.get(23)?;
    let attachment_thumb_mime: Option<String> = row.get(24)?;
    let attachment_waveform: Option<Vec<u8>> = row.get(25)?;
    let attachment = match (attachment_kind, attachment_data, attachment_blob_ref) {
        (Some(kind), data, blob_ref) => {
            let data = data.unwrap_or_default();
            Some(Attachment {
                kind: AttachmentKind::parse(&kind).unwrap_or(AttachmentKind::File),
                name: attachment_name.unwrap_or_default(),
                mime: attachment_mime.unwrap_or_default(),
                size: attachment_size
                    .map(from_sql_u64)
                    .unwrap_or(data.len() as u64),
                blob_ref,
                key: attachment_key,
                nonce: attachment_nonce,
                thumb: attachment_thumb_ref.map(|reference| BlobRef {
                    reference,
                    key: attachment_thumb_key.clone().unwrap_or_default(),
                    nonce: attachment_thumb_nonce.clone().unwrap_or_default(),
                    size: attachment_thumb_size.map(from_sql_u64).unwrap_or(0),
                    mime: attachment_thumb_mime.clone().unwrap_or_default(),
                    data: Vec::new(),
                }),
                waveform: attachment_waveform.unwrap_or_default(),
                data,
            })
        }
        _ => None,
    };
    Ok(MessageRecord {
        local_id: row.get(0)?,
        server_id: server_id.map(from_sql_u64),
        chat_id: row.get(2)?,
        sender_user_id: row.get(3)?,
        sender_device_id: row.get(4)?,
        content_kind: row.get(5)?,
        body: row.get(6)?,
        attachment,
        reply_to_server_id: reply_to.map(from_sql_u64),
        grouped_id: row.get(26)?,
        status: status.and_then(MessageStatus::from_i64),
        created_at_ms: from_sql_u64(row.get(9)?),
        edited_at_ms: edited.map(from_sql_u64),
        deleted_at_ms: deleted.map(from_sql_u64),
    })
}

pub(crate) const MESSAGE_COLUMNS: &str = "local_id, server_id, chat_id, sender_user_id, \
     sender_device_id, content_kind, body, reply_to_server_id, status, created_at_ms, \
     edited_at_ms, deleted_at_ms, attachment_kind, attachment_name, attachment_mime, \
     attachment_size, attachment_data, attachment_blob_ref, attachment_key, \
     attachment_nonce, attachment_thumb_ref, attachment_thumb_key, \
     attachment_thumb_nonce, attachment_thumb_size, attachment_thumb_mime, \
     attachment_waveform, grouped_id";

/// Строка `outbox` → доменная запись. Позиции колонок фиксированы
/// константой [`OUTBOX_COLUMNS`].
pub(crate) fn outbox_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OutboxRecord> {
    let state: i64 = row.get(4)?;
    let last_error_code: Option<i64> = row.get(7)?;
    let server_message_id: Option<i64> = row.get(10)?;
    Ok(OutboxRecord {
        local_id: row.get(0)?,
        chat_id: row.get(1)?,
        idempotency_key: row.get(2)?,
        request: row.get(3)?,
        state: rchat_core::types::OutboxState::from_i64(state)
            .unwrap_or(rchat_core::types::OutboxState::Pending),
        attempts: from_sql_u64(row.get(5)?) as u32,
        next_attempt_at_ms: from_sql_u64(row.get(6)?),
        last_error_code: last_error_code.map(|value| value as i32),
        last_error: row.get(8)?,
        created_at_ms: from_sql_u64(row.get(9)?),
        server_message_id: server_message_id.map(from_sql_u64),
    })
}

pub(crate) const OUTBOX_COLUMNS: &str = "local_id, chat_id, idempotency_key, request, state, \
     attempts, next_attempt_at_ms, last_error_code, last_error, created_at_ms, server_message_id";

pub(crate) fn user_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<UserRecord> {
    Ok(UserRecord {
        user_id: row.get(0)?,
        username: row.get(1)?,
        display_name: row.get(2)?,
    })
}

pub(crate) fn member_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemberRecord> {
    let role: i64 = row.get(3)?;
    Ok(MemberRecord {
        user_id: row.get(0)?,
        username: row.get(1)?,
        display_name: row.get(2)?,
        role: proto::ChatMemberRole::try_from(role as i32)
            .unwrap_or(proto::ChatMemberRole::Unspecified),
        joined_at_ms: from_sql_u64(row.get(4)?),
    })
}

pub(crate) fn device_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeviceRecord> {
    let revoked: Option<i64> = row.get(7)?;
    Ok(DeviceRecord {
        device_id: row.get(0)?,
        user_id: row.get(1)?,
        name: row.get(2)?,
        platform: row.get(3)?,
        e2e_version: from_sql_u64(row.get(4)?) as u32,
        ed25519: row.get(5)?,
        curve25519: row.get(6)?,
        revoked_at_ms: revoked.map(from_sql_u64),
    })
}

pub(crate) const DEVICE_COLUMNS: &str =
    "device_id, user_id, name, platform, e2e_version, ed25519, curve25519, revoked_at_ms";

pub(crate) fn receipt_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReceiptRecord> {
    Ok(ReceiptRecord {
        chat_id: row.get(0)?,
        user_id: row.get(1)?,
        delivered_up_to: from_sql_u64(row.get(2)?),
        read_up_to: from_sql_u64(row.get(3)?),
        updated_at_ms: from_sql_u64(row.get(4)?),
    })
}

pub(crate) const RECEIPT_COLUMNS: &str =
    "chat_id, user_id, delivered_up_to, read_up_to, updated_at_ms";

/// SQLite-ошибка → доменная ошибка. Конфликты уникальности распознаются по
/// имени индекса/таблицы, чтобы вызывающий код понимал причину.
pub(crate) fn map_sql(context: &str, error: rusqlite::Error) -> StorageError {
    if let rusqlite::Error::SqliteFailure(err, Some(message)) = &error {
        if err.code == rusqlite::ErrorCode::ConstraintViolation {
            return StorageError::Constraint(format!("{context}: {message}"));
        }
    }
    StorageError::Internal(format!("{context}: {error}"))
}

/// Экранирует спецсимволы LIKE (`%`, `_`, `\`) для префиксных выборок.
pub(crate) fn escape_like(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(ch);
            }
            other => out.push(other),
        }
    }
    out
}

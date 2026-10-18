//! Преобразование строк SQLite в доменные записи.

use rchat_server::model::{
    ChatRecord, DeviceRecord, MemberRecord, MessageRecord, PreKeyRecord, RecipientRecord,
    SessionRecord, SyncEventRecord, UserRecord,
};
use rusqlite::Row;

pub(crate) fn user_from_row(row: &Row<'_>) -> rusqlite::Result<UserRecord> {
    Ok(UserRecord {
        user_id: row.get("user_id")?,
        username: row.get("username")?,
        display_name: row.get("display_name")?,
        password_hash: row.get("password_hash")?,
        created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
        updated_at_ms: row.get::<_, i64>("updated_at_ms")? as u64,
        disabled: row.get::<_, i64>("disabled")? != 0,
    })
}

pub(crate) fn device_from_row(row: &Row<'_>) -> rusqlite::Result<DeviceRecord> {
    Ok(DeviceRecord {
        device_id: row.get("device_id")?,
        user_id: row.get("user_id")?,
        name: row.get("name")?,
        platform: row.get("platform")?,
        e2e_version: row.get::<_, i64>("e2e_version")? as u32,
        ed25519: row.get("ed25519")?,
        curve25519: row.get("curve25519")?,
        created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
        last_seen_at_ms: row.get::<_, i64>("last_seen_at_ms")? as u64,
        revoked_at_ms: row.get::<_, i64>("revoked_at_ms")? as u64,
    })
}

pub(crate) fn session_from_row(row: &Row<'_>) -> rusqlite::Result<SessionRecord> {
    Ok(SessionRecord {
        session_id: row.get("session_id")?,
        token_hash: row.get("token_hash")?,
        user_id: row.get("user_id")?,
        device_id: row.get("device_id")?,
        created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
        expires_at_ms: row.get::<_, i64>("expires_at_ms")? as u64,
        last_used_at_ms: row.get::<_, i64>("last_used_at_ms")? as u64,
        revoked_at_ms: row.get::<_, i64>("revoked_at_ms")? as u64,
    })
}

pub(crate) fn prekey_from_row(row: &Row<'_>) -> rusqlite::Result<PreKeyRecord> {
    Ok(PreKeyRecord {
        key_id: row.get::<_, i64>("key_id")? as u32,
        public_key: row.get("public_key")?,
        fallback: row.get::<_, i64>("fallback")? != 0,
        created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
    })
}

pub(crate) fn chat_from_row(row: &Row<'_>) -> rusqlite::Result<ChatRecord> {
    Ok(ChatRecord {
        chat_id: row.get("chat_id")?,
        kind: row.get::<_, i64>("kind")? as i32,
        e2e_mode: row.get::<_, i64>("e2e_mode")? as i32,
        name: row.get("name")?,
        created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
        updated_at_ms: row.get::<_, i64>("updated_at_ms")? as u64,
        last_server_id: row.get::<_, i64>("last_server_id")? as u64,
    })
}

pub(crate) fn member_from_row(row: &Row<'_>) -> rusqlite::Result<MemberRecord> {
    Ok(MemberRecord {
        chat_id: row.get("chat_id")?,
        user_id: row.get("user_id")?,
        joined_at_ms: row.get::<_, i64>("joined_at_ms")? as u64,
        role: row.get::<_, i64>("role")? as i32,
    })
}

pub(crate) fn message_from_row(row: &Row<'_>) -> rusqlite::Result<MessageRecord> {
    Ok(MessageRecord {
        server_id: row.get::<_, i64>("server_id")? as u64,
        chat_id: row.get("chat_id")?,
        sender_user_id: row.get("sender_user_id")?,
        sender_device_id: row.get("sender_device_id")?,
        client_message_id: row.get("client_message_id")?,
        content_kind: row.get("content_kind")?,
        reply_to_server_id: row.get::<_, i64>("reply_to_server_id")? as u64,
        created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
        edited_at_ms: row.get::<_, i64>("edited_at_ms")? as u64,
        deleted_at_ms: row.get::<_, i64>("deleted_at_ms")? as u64,
        sender_curve25519: row.get("sender_curve25519")?,
        olm_type: row.get::<_, i64>("olm_type")? as i32,
        plaintext_dev: row.get("plaintext_dev")?,
        group_ciphertext: row.get("group_ciphertext")?,
        group_session_id: row.get("group_session_id")?,
    })
}

pub(crate) fn recipient_from_row(row: &Row<'_>) -> rusqlite::Result<RecipientRecord> {
    Ok(RecipientRecord {
        server_id: row.get::<_, i64>("server_id")? as u64,
        device_id: row.get("device_id")?,
        ciphertext: row.get("ciphertext")?,
        e2e_version: row.get::<_, i64>("e2e_version")? as u32,
        olm_type: row.get::<_, i64>("olm_type")? as i32,
        sender_curve25519: row.get("sender_curve25519")?,
        payload_kind: row.get::<_, i64>("payload_kind")? as i32,
    })
}

pub(crate) fn sync_event_from_row(row: &Row<'_>) -> rusqlite::Result<SyncEventRecord> {
    Ok(SyncEventRecord {
        device_id: row.get("device_id")?,
        seq: row.get::<_, i64>("seq")? as u64,
        kind_hint: row.get::<_, i64>("kind_hint")? as u32,
        payload: row.get("payload")?,
        created_at_ms: row.get::<_, i64>("created_at_ms")? as u64,
    })
}

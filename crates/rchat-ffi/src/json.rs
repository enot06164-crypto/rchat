//! Преобразование доменных записей и UI-событий в стабильный JSON.
//!
//! Схема описана в `docs/ffi.md`. Все бинарные ключи устройств кодируются
//! hex: JSON не предназначен для передачи секретов, а открытые identity-ключи
//! и так публичны.

use rchat_core::avatar::Avatar;
use rchat_core::events::UiEvent;
use rchat_core::types::{
    CallRecord, CallState, ChatRecord, ConnectionState, DeviceRecord, LocalAccount, MemberRecord,
    MessageRecord, MessageStatus, OutboxState, ReceiptRecord, SyncProgress,
};
use rchat_proto::proto::{ChatKind, ChatMemberRole, E2eMode};
use serde_json::{json, Value};

pub(crate) fn connection_state(state: ConnectionState) -> &'static str {
    match state {
        ConnectionState::Disconnected => "disconnected",
        ConnectionState::Connecting => "connecting",
        ConnectionState::Unauthenticated => "unauthenticated",
        ConnectionState::Connected => "connected",
        ConnectionState::Reconnecting => "reconnecting",
    }
}

fn chat_kind(kind: ChatKind) -> &'static str {
    match kind {
        ChatKind::Unspecified => "unspecified",
        ChatKind::Direct => "direct",
        ChatKind::Group => "group",
    }
}

fn e2e_mode(mode: E2eMode) -> &'static str {
    match mode {
        E2eMode::Unspecified => "unspecified",
        E2eMode::NoneDev => "none_dev",
        E2eMode::OlmV1 => "olm_v1",
    }
}

fn member_role(role: ChatMemberRole) -> &'static str {
    match role {
        ChatMemberRole::Unspecified => "unspecified",
        ChatMemberRole::Member => "member",
        ChatMemberRole::Owner => "owner",
    }
}

fn message_status(status: Option<MessageStatus>) -> &'static str {
    match status {
        None => "none",
        Some(MessageStatus::Pending) => "pending",
        Some(MessageStatus::Sending) => "sending",
        Some(MessageStatus::Sent) => "sent",
        Some(MessageStatus::Failed) => "failed",
    }
}

pub(crate) fn outbox_state(state: OutboxState) -> &'static str {
    match state {
        OutboxState::Pending => "pending",
        OutboxState::Sending => "sending",
        OutboxState::Sent => "sent",
        OutboxState::Failed => "failed",
    }
}

fn call_state(state: CallState) -> &'static str {
    match state {
        CallState::Ringing => "ringing",
        CallState::Active => "active",
        CallState::Unknown => "unknown",
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// Вложенный объект `avatar` для пользователей и чатов.
pub(crate) fn avatar(id: &str, display_name: &str, fallback: &str) -> Value {
    let avatar = Avatar::new(id, display_name, fallback);
    json!({
        "initials": avatar.initials,
        "color_index": avatar.color_index,
        "color_hex": avatar.color_hex,
    })
}

pub(crate) fn chat(chat: &ChatRecord) -> Value {
    json!({
        "chat_id": chat.chat_id,
        "kind": chat_kind(chat.kind),
        "e2e_mode": e2e_mode(chat.e2e_mode),
        "title": chat.title,
        "peer_user_id": chat.peer_user_id,
        "created_at_ms": chat.created_at_ms,
        "updated_at_ms": chat.updated_at_ms,
        "last_server_id": chat.last_server_id,
        "avatar": avatar(&chat.chat_id, &chat.title, &chat.title),
    })
}

pub(crate) fn member(member: &MemberRecord) -> Value {
    json!({
        "user_id": member.user_id,
        "username": member.username,
        "display_name": member.display_name,
        "role": member_role(member.role),
        "joined_at_ms": member.joined_at_ms,
        "avatar": avatar(&member.user_id, &member.display_name, &member.username),
    })
}

pub(crate) fn message(message: &MessageRecord) -> Value {
    json!({
        "local_id": message.local_id,
        "server_id": message.server_id,
        "chat_id": message.chat_id,
        "sender_user_id": message.sender_user_id,
        "sender_device_id": message.sender_device_id,
        "content_kind": message.content_kind,
        "body": message.body,
        "reply_to_server_id": message.reply_to_server_id,
        "status": message_status(message.status),
        "created_at_ms": message.created_at_ms,
        "edited_at_ms": message.edited_at_ms,
        "deleted_at_ms": message.deleted_at_ms,
    })
}

pub(crate) fn device(device: &DeviceRecord) -> Value {
    json!({
        "device_id": device.device_id,
        "user_id": device.user_id,
        "name": device.name,
        "platform": device.platform,
        "e2e_version": device.e2e_version,
        "ed25519": hex(&device.ed25519),
        "curve25519": hex(&device.curve25519),
        "revoked_at_ms": device.revoked_at_ms,
    })
}

pub(crate) fn receipt(receipt: &ReceiptRecord) -> Value {
    json!({
        "chat_id": receipt.chat_id,
        "user_id": receipt.user_id,
        "delivered_up_to": receipt.delivered_up_to,
        "read_up_to": receipt.read_up_to,
        "updated_at_ms": receipt.updated_at_ms,
    })
}

pub(crate) fn call(call: &CallRecord) -> Value {
    json!({
        "call_id": call.call_id,
        "chat_id": call.chat_id,
        "caller_user_id": call.caller_user_id,
        "callee_user_id": call.callee_user_id,
        "state": call_state(call.state),
        "created_at_ms": call.created_at_ms,
        "accepted_at_ms": call.accepted_at_ms,
    })
}

pub(crate) fn progress(progress: SyncProgress) -> Value {
    json!({
        "from_seq": progress.from_seq,
        "to_seq": progress.to_seq,
        "events_applied": progress.events_applied,
        "events_skipped": progress.events_skipped,
        "resync_performed": progress.resync_performed,
    })
}

pub(crate) fn account(account: &LocalAccount) -> Value {
    json!({
        "user_id": account.user_id,
        "username": account.username,
        "display_name": account.display_name,
        "device_id": account.device_id,
        "avatar": avatar(&account.user_id, &account.display_name, &account.username),
    })
}

/// Событие в формате `{"@type":"event","event":"...","data":...}`.
pub(crate) fn event(event: &UiEvent) -> Value {
    let (name, data) = match event {
        UiEvent::ConnectionStateChanged(state) => (
            "connection_state_changed",
            json!({"state": connection_state(*state)}),
        ),
        UiEvent::SessionStarted { user_id, username } => (
            "session_started",
            json!({"user_id": user_id, "username": username}),
        ),
        UiEvent::SessionEnded { reason } => ("session_ended", json!({"reason": reason})),
        UiEvent::MessageReceived(message) => ("message_received", self::message(message)),
        UiEvent::MessageUpdated(message) => ("message_updated", self::message(message)),
        UiEvent::OutboxChanged {
            local_id,
            state,
            error,
        } => (
            "outbox_changed",
            json!({
                "local_id": local_id,
                "state": outbox_state(*state),
                "error": error,
            }),
        ),
        UiEvent::ChatUpserted(chat) => ("chat_upserted", self::chat(chat)),
        UiEvent::ReceiptUpdated(receipt) => ("receipt_updated", self::receipt(receipt)),
        UiEvent::PresenceChanged { user_id, online } => (
            "presence_changed",
            json!({"user_id": user_id, "online": online}),
        ),
        UiEvent::CallInvited(call) => ("call_invited", self::call(call)),
        UiEvent::CallAccepted(call) => ("call_accepted", self::call(call)),
        UiEvent::CallDeclined(call) => ("call_declined", self::call(call)),
        UiEvent::CallEnded(call) => ("call_ended", self::call(call)),
        UiEvent::SyncProgress(progress) => ("sync_progress", self::progress(*progress)),
        UiEvent::ResyncPerformed { up_to_seq } => {
            ("resync_performed", json!({"up_to_seq": up_to_seq}))
        }
        UiEvent::E2eDecryptFailed {
            chat_id,
            server_id,
            device_id,
        } => (
            "e2e_decrypt_failed",
            json!({
                "chat_id": chat_id,
                "server_id": server_id,
                "device_id": device_id,
            }),
        ),
        UiEvent::Warning(message) => ("warning", json!({"message": message})),
    };
    json!({
        "@type": "event",
        "event": name,
        "data": data,
    })
}

pub(crate) fn lagged_event(missed: u64) -> Vec<u8> {
    let value = json!({
        "@type": "event",
        "event": "event_lagged",
        "data": {"missed": missed},
    });
    serde_json::to_vec(&value).expect("JSON value is serializable")
}

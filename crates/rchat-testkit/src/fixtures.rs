//! Детерминированные фикстуры доменных записей ядра и протокола.
//!
//! Значения по умолчанию: согласованные временные метки от [`BASE_MS`],
//! предсказуемые ключи (первый байт — 0x11/0x22) и простые идентификаторы.
//! Генераторы не скрывают поля: тест может переопределить любое значение
//! через `..`-синтаксис или мутацию.

use rchat_core::types::{
    ChatRecord, DeviceRecord, LocalAccount, MemberRecord, MessageRecord, MessageStatus,
    OutboxRecord, OutboxState, UserRecord,
};
use rchat_proto::proto;

/// Базовая временная метка фикстур (2023-11-14); смещения — в секундах.
pub const BASE_MS: u64 = 1_700_000_000_000;

/// Метка времени через `seconds` секунд после [`BASE_MS`].
pub fn at(seconds: u64) -> u64 {
    BASE_MS + seconds * 1000
}

pub fn account(user_id: &str, username: &str, device_id: &str) -> LocalAccount {
    LocalAccount {
        user_id: user_id.to_string(),
        username: username.to_string(),
        display_name: format!("display-{username}"),
        device_id: device_id.to_string(),
    }
}

pub fn user(user_id: &str, username: &str) -> UserRecord {
    UserRecord {
        user_id: user_id.to_string(),
        username: username.to_string(),
        display_name: format!("display-{username}"),
    }
}

/// Устройство с заполненными публичными ключами (`seed` — первый байт).
pub fn device(device_id: &str, user_id: &str, seed: u8) -> DeviceRecord {
    DeviceRecord {
        device_id: device_id.to_string(),
        user_id: user_id.to_string(),
        name: format!("device-{device_id}"),
        platform: "test".to_string(),
        e2e_version: 1,
        ed25519: vec![seed; 32],
        curve25519: vec![seed.wrapping_add(1); 32],
        revoked_at_ms: None,
    }
}

pub fn direct_chat(chat_id: &str, peer_user_id: &str) -> ChatRecord {
    ChatRecord {
        chat_id: chat_id.to_string(),
        kind: proto::ChatKind::Direct,
        e2e_mode: proto::E2eMode::NoneDev,
        title: String::new(),
        peer_user_id: Some(peer_user_id.to_string()),
        created_at_ms: BASE_MS,
        updated_at_ms: BASE_MS,
        last_server_id: 0,
    }
}

pub fn member(user_id: &str, username: &str) -> MemberRecord {
    MemberRecord {
        user_id: user_id.to_string(),
        username: username.to_string(),
        display_name: format!("display-{username}"),
        role: proto::ChatMemberRole::Unspecified,
        joined_at_ms: BASE_MS,
    }
}

/// Подтверждённое входящее сообщение (`server_id` заполнен).
pub fn incoming_message(
    server_id: u64,
    chat_id: &str,
    sender_user_id: &str,
    body: &str,
) -> MessageRecord {
    MessageRecord {
        local_id: format!("s:{server_id}"),
        server_id: Some(server_id),
        chat_id: chat_id.to_string(),
        sender_user_id: sender_user_id.to_string(),
        sender_device_id: format!("{sender_user_id}-device"),
        content_kind: "text".to_string(),
        body: Some(body.to_string()),
        reply_to_server_id: None,
        status: None,
        created_at_ms: at(server_id),
        edited_at_ms: None,
        deleted_at_ms: None,
    }
}

/// Собственное сообщение в очереди отправки (пара «сообщение + outbox»).
pub fn outgoing_message(
    local_id: &str,
    chat_id: &str,
    sender_user_id: &str,
    body: &str,
) -> (MessageRecord, OutboxRecord) {
    let message = MessageRecord {
        local_id: local_id.to_string(),
        server_id: None,
        chat_id: chat_id.to_string(),
        sender_user_id: sender_user_id.to_string(),
        sender_device_id: format!("{sender_user_id}-device"),
        content_kind: "text".to_string(),
        body: Some(body.to_string()),
        reply_to_server_id: None,
        status: Some(MessageStatus::Pending),
        created_at_ms: BASE_MS,
        edited_at_ms: None,
        deleted_at_ms: None,
    };
    let outbox = OutboxRecord {
        local_id: local_id.to_string(),
        chat_id: chat_id.to_string(),
        idempotency_key: local_id.to_string(),
        request: Vec::new(),
        state: OutboxState::Pending,
        attempts: 0,
        next_attempt_at_ms: 0,
        last_error_code: None,
        last_error: None,
        created_at_ms: BASE_MS,
        server_message_id: None,
    };
    (message, outbox)
}

/// `DeviceInfo` для серверных тестов: с ключами (`seed`) или без.
pub fn device_info(device_id: &str, seed: Option<u8>) -> proto::DeviceInfo {
    proto::DeviceInfo {
        device_id: device_id.to_string(),
        name: format!("device-{device_id}"),
        platform: "test".to_string(),
        keys: seed.map(|seed| proto::DeviceKeys {
            ed25519: vec![seed; 32],
            curve25519: vec![seed.wrapping_add(1); 32],
        }),
        e2e_version: if seed.is_some() { 1 } else { 0 },
        created_at_ms: 0,
        last_seen_at_ms: 0,
        revoked_at_ms: 0,
        user_id: String::new(),
    }
}

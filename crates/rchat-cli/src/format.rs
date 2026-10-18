//! Форматирование записей и событий для терминала.

use rchat_core::types::{
    ChatRecord, ConnectionState, DeviceRecord, MessageRecord, MessageStatus, OutboxState,
    ReceiptRecord,
};
use rchat_proto::proto;

/// Время в RFC 3339 (UTC). При выходе за границы диапазона — миллисекунды.
pub fn timestamp(ms: u64) -> String {
    let seconds = (ms / 1000) as i64;
    match time::OffsetDateTime::from_unix_timestamp(seconds) {
        Ok(value) => value
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| format!("{ms}ms")),
        Err(_) => format!("{ms}ms"),
    }
}

pub fn connection_state(state: ConnectionState) -> &'static str {
    match state {
        ConnectionState::Disconnected => "нет соединения",
        ConnectionState::Connecting => "подключение…",
        ConnectionState::Unauthenticated => "подключён без сессии (нужен вход)",
        ConnectionState::Connected => "подключён, сессия активна",
        ConnectionState::Reconnecting => "переподключение…",
    }
}

pub fn chat_kind(kind: proto::ChatKind) -> &'static str {
    match kind {
        proto::ChatKind::Direct => "личный",
        other => {
            let _ = other;
            "неизвестный"
        }
    }
}

pub fn e2e_mode(mode: proto::E2eMode) -> &'static str {
    match mode {
        proto::E2eMode::OlmV1 => "E2E (olm.v1)",
        proto::E2eMode::NoneDev => "без E2E (dev)",
        other => {
            let _ = other;
            "неизвестный режим"
        }
    }
}

pub fn message_status(status: Option<MessageStatus>) -> &'static str {
    match status {
        None => "",
        Some(MessageStatus::Pending) => " [в очереди]",
        Some(MessageStatus::Sending) => " [отправляется]",
        Some(MessageStatus::Sent) => " [отправлено]",
        Some(MessageStatus::Failed) => " [ошибка]",
    }
}

pub fn outbox_state(state: OutboxState) -> &'static str {
    match state {
        OutboxState::Pending => "в очереди",
        OutboxState::Sending => "отправляется",
        OutboxState::Sent => "отправлено",
        OutboxState::Failed => "ошибка",
    }
}

pub fn message_line(message: &MessageRecord) -> String {
    let id = match message.server_id {
        Some(server_id) => format!("#{server_id}"),
        None => format!("локальное {}", message.local_id),
    };
    let body = match (&message.body, message.deleted_at_ms) {
        (_, Some(_)) => "(удалено)".to_string(),
        (Some(text), None) => text.clone(),
        (None, None) => "(не удалось расшифровать)".to_string(),
    };
    let mut line = format!(
        "{} {} {}: {}{}",
        timestamp(message.created_at_ms),
        id,
        message.sender_user_id,
        body,
        message_status(message.status)
    );
    if let Some(edited) = message.edited_at_ms {
        line.push_str(&format!(" (изменено {})", timestamp(edited)));
    }
    if let Some(reply) = message.reply_to_server_id {
        line.push_str(&format!(" (ответ на #{reply})"));
    }
    line
}

pub fn chat_line(chat: &ChatRecord) -> String {
    format!(
        "{} — {} ({}, {}), последнее сообщение: {}{}",
        chat.chat_id,
        chat.title,
        chat_kind(chat.kind),
        e2e_mode(chat.e2e_mode),
        if chat.last_server_id == 0 {
            "нет".to_string()
        } else {
            format!("#{}", chat.last_server_id)
        },
        if chat.updated_at_ms == 0 {
            String::new()
        } else {
            format!(" ({})", timestamp(chat.updated_at_ms))
        }
    )
}

pub fn device_line(device: &DeviceRecord) -> String {
    let mut line = format!(
        "{} — {} ({})",
        device.device_id, device.name, device.platform
    );
    if let Some(fingerprint) = key_fingerprint(device) {
        line.push_str(&format!(" [ключ: {fingerprint}]"));
    }
    if let Some(revoked) = device.revoked_at_ms {
        line.push_str(&format!(" [отозвано {}]", timestamp(revoked)));
    }
    line
}

/// Отпечаток identity-ключей устройства: SHA-256 от `ed25519 || curve25519`,
/// первые 16 байт четырьмя группами hex.
///
/// Сравнение отпечатков по независимому каналу подтверждает идентичность
/// устройства собеседника (см. `docs/threat-model.md`).
pub fn key_fingerprint(device: &DeviceRecord) -> Option<String> {
    use sha2::Digest as _;

    if device.ed25519.is_empty() || device.curve25519.is_empty() {
        return None;
    }
    let mut hasher = sha2::Sha256::new();
    hasher.update(&device.ed25519);
    hasher.update(&device.curve25519);
    let digest = hasher.finalize();
    let groups: Vec<String> = digest[..16]
        .chunks(4)
        .map(|chunk| chunk.iter().map(|byte| format!("{byte:02x}")).collect())
        .collect();
    Some(groups.join(" "))
}

pub fn receipt_line(receipt: &ReceiptRecord) -> String {
    format!(
        "{}: доставлено до #{}, прочитано до #{} ({})",
        receipt.user_id,
        receipt.delivered_up_to,
        receipt.read_up_to,
        timestamp(receipt.updated_at_ms)
    )
}

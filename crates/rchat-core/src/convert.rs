//! Конвертация между wire-типами (`rchat-proto`) и доменными записями клиента.

use crate::payload::Payload;
use rchat_proto::proto;

use crate::error::CoreError;
use crate::traits::{OneTimeKey, PeerDevice};
use crate::types::{
    CallRecord, CallState, ChatRecord, DeviceRecord, MemberRecord, MessageRecord, UserRecord,
};

/// Пакеты ключей из ответа сервера в доменные дескрипторы устройств.
pub fn peer_devices_from_bundles(bundles: &[proto::DeviceBundle]) -> Vec<PeerDevice> {
    bundles
        .iter()
        .map(|bundle| {
            let keys = bundle.keys.clone().unwrap_or_default();
            PeerDevice {
                user_id: bundle.user_id.clone(),
                device_id: bundle.device_id.clone(),
                ed25519: keys.ed25519,
                curve25519: keys.curve25519,
                one_time_key: bundle
                    .one_time_key
                    .as_ref()
                    .map(|key| OneTimeKey::from_proto(key, bundle.fallback)),
            }
        })
        .collect()
}

/// Локальный идентификатор принятого с сервера сообщения: производный от
/// server_id, что обеспечивает идемпотентность повторной доставки.
pub fn server_message_local_id(server_id: u64) -> String {
    format!("s:{server_id}")
}

/// Локальный идентификатор собственного исходящего сообщения.
pub fn new_local_message_id() -> Result<String, CoreError> {
    rchat_proto::random_id_hex(16).map_err(|e| CoreError::Internal(format!("rng: {e}")))
}

pub fn user_from_proto(user: &proto::UserInfo) -> UserRecord {
    UserRecord {
        user_id: user.user_id.clone(),
        username: user.username.clone(),
        display_name: user.display_name.clone(),
    }
}

pub fn device_from_proto(device: &proto::DeviceInfo) -> Result<DeviceRecord, CoreError> {
    let (ed25519, curve25519) = match device.keys.as_ref() {
        Some(keys) => (keys.ed25519.clone(), keys.curve25519.clone()),
        None => (Vec::new(), Vec::new()),
    };
    if device.e2e_version != rchat_proto::E2E_VERSION_NONE
        && (ed25519.len() != 32 || curve25519.len() != 32)
    {
        return Err(CoreError::Protocol(format!(
            "device {} has e2e_version {} but invalid keys",
            device.device_id, device.e2e_version
        )));
    }
    Ok(DeviceRecord {
        device_id: device.device_id.clone(),
        user_id: device.user_id.clone(),
        name: device.name.clone(),
        platform: device.platform.clone(),
        e2e_version: device.e2e_version,
        ed25519,
        curve25519,
        revoked_at_ms: if device.revoked_at_ms == 0 {
            None
        } else {
            Some(device.revoked_at_ms)
        },
    })
}

pub fn member_from_proto(member: &proto::ChatMember) -> MemberRecord {
    MemberRecord {
        user_id: member.user_id.clone(),
        username: member.username.clone(),
        display_name: member.display_name.clone(),
        role: proto::ChatMemberRole::try_from(member.role)
            .unwrap_or(proto::ChatMemberRole::Unspecified),
        joined_at_ms: member.joined_at_ms,
    }
}

/// Преобразует чат из протокола в доменную запись, определяя собеседника и
/// заголовок с точки зрения пользователя `as_user`: у личного чата заголовок —
/// имя собеседника, у группового — имя группы.
pub fn chat_from_proto(
    chat: &proto::Chat,
    as_user: &str,
) -> Result<(ChatRecord, Vec<MemberRecord>), CoreError> {
    let kind = proto::ChatKind::try_from(chat.kind)
        .map_err(|_| CoreError::Protocol(format!("unknown chat kind {}", chat.kind)))?;
    let e2e_mode = proto::E2eMode::try_from(chat.e2e_mode)
        .map_err(|_| CoreError::Protocol(format!("unknown e2e mode {}", chat.e2e_mode)))?;
    if kind == proto::ChatKind::Unspecified || e2e_mode == proto::E2eMode::Unspecified {
        return Err(CoreError::Protocol("chat without kind/e2e mode".into()));
    }
    let members: Vec<MemberRecord> = chat.members.iter().map(member_from_proto).collect();
    let (title, peer_user_id) = match kind {
        proto::ChatKind::Group => {
            let title = if chat.name.is_empty() {
                "group".to_string()
            } else {
                chat.name.clone()
            };
            (title, None)
        }
        _ => {
            let peer = members.iter().find(|m| m.user_id != as_user);
            let title = match peer {
                Some(m) if !m.display_name.is_empty() => m.display_name.clone(),
                Some(m) => m.username.clone(),
                None => "chat".to_string(),
            };
            (title, peer.map(|m| m.user_id.clone()))
        }
    };
    let record = ChatRecord {
        chat_id: chat.chat_id.clone(),
        kind,
        e2e_mode,
        cloud_key: chat.cloud_key.clone(),
        title,
        peer_user_id,
        created_at_ms: chat.created_at_ms,
        updated_at_ms: chat.updated_at_ms,
        last_server_id: chat.last_server_id,
    };
    Ok((record, members))
}

/// Преобразует сообщение протокола в доменную запись. `payload` — результат
/// расшифрования (или plaintext dev-режима): конверт с текстом и вложением;
/// `None` — расшифровать не удалось.
pub fn message_from_proto(message: &proto::Message, payload: Option<Payload>) -> MessageRecord {
    let body = payload.as_ref().map(|p| p.text.clone());
    let attachment = payload.as_ref().and_then(|p| p.attachment.clone());
    let content_kind = payload
        .as_ref()
        .map(|p| p.content_kind())
        .unwrap_or_else(|| message.content_kind.clone());
    MessageRecord {
        local_id: server_message_local_id(message.server_id),
        server_id: Some(message.server_id),
        chat_id: message.chat_id.clone(),
        sender_user_id: message.sender_user_id.clone(),
        sender_device_id: message.sender_device_id.clone(),
        content_kind,
        body,
        attachment,
        reply_to_server_id: if message.reply_to_server_id == 0 {
            None
        } else {
            Some(message.reply_to_server_id)
        },
        grouped_id: if message.grouped_id.is_empty() {
            None
        } else {
            Some(message.grouped_id.clone())
        },
        status: None,
        created_at_ms: message.created_at_ms,
        edited_at_ms: if message.edited_at_ms == 0 {
            None
        } else {
            Some(message.edited_at_ms)
        },
        deleted_at_ms: if message.deleted_at_ms == 0 {
            None
        } else {
            Some(message.deleted_at_ms)
        },
    }
}

/// Конвертирует звонок из протокола. Неизвестное состояние сохраняется как
/// `CallState::Unknown`, чтобы будущие версии не роняли клиент.
pub fn call_from_proto(call: &proto::Call) -> CallRecord {
    CallRecord {
        call_id: call.call_id.clone(),
        chat_id: call.chat_id.clone(),
        caller_user_id: call.caller_user_id.clone(),
        callee_user_id: call.callee_user_id.clone(),
        state: match proto::CallState::try_from(call.state) {
            Ok(proto::CallState::Ringing) => CallState::Ringing,
            Ok(proto::CallState::Active) => CallState::Active,
            _ => CallState::Unknown,
        },
        created_at_ms: call.created_at_ms,
        accepted_at_ms: call.accepted_at_ms,
    }
}

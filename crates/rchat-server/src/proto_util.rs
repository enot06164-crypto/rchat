//! Конвертация доменных записей сервера в протокольные сообщения и обратно,
//! а также сборка/разбор событий журнала синхронизации.

use rchat_proto::proto;
use rchat_proto::proto::sync_event;

use crate::calls::{ActiveCall, CallState};
use crate::error::ServerError;
use crate::model::{
    ChatRecord, DeviceRecord, MemberRecord, MessageRecord, ReceiptRecord, RecipientRecord,
    UserRecord,
};

pub fn user_to_proto(user: &UserRecord) -> proto::UserInfo {
    proto::UserInfo {
        user_id: user.user_id.clone(),
        username: user.username.clone(),
        display_name: user.display_name.clone(),
    }
}

pub fn device_to_proto(device: &DeviceRecord) -> proto::DeviceInfo {
    proto::DeviceInfo {
        device_id: device.device_id.clone(),
        name: device.name.clone(),
        platform: device.platform.clone(),
        keys: if device.has_e2e_keys() {
            Some(proto::DeviceKeys {
                ed25519: device.ed25519.clone(),
                curve25519: device.curve25519.clone(),
            })
        } else {
            None
        },
        e2e_version: device.e2e_version,
        created_at_ms: device.created_at_ms,
        last_seen_at_ms: device.last_seen_at_ms,
        revoked_at_ms: device.revoked_at_ms,
        user_id: device.user_id.clone(),
    }
}

pub fn member_to_proto(member: &MemberRecord, user: &UserRecord) -> proto::ChatMember {
    proto::ChatMember {
        user_id: member.user_id.clone(),
        username: user.username.clone(),
        display_name: user.display_name.clone(),
        joined_at_ms: member.joined_at_ms,
        role: member.role,
    }
}

pub fn chat_to_proto(
    chat: &ChatRecord,
    members: &[MemberRecord],
    users: &[UserRecord],
) -> proto::Chat {
    let members_proto = members
        .iter()
        .map(|member| {
            let user = users
                .iter()
                .find(|user| user.user_id == member.user_id)
                .cloned()
                .unwrap_or_else(|| UserRecord {
                    user_id: member.user_id.clone(),
                    username: String::new(),
                    display_name: String::new(),
                    password_hash: String::new(),
                    created_at_ms: 0,
                    updated_at_ms: 0,
                    disabled: true,
                });
            member_to_proto(member, &user)
        })
        .collect();
    proto::Chat {
        chat_id: chat.chat_id.clone(),
        kind: chat.kind,
        e2e_mode: chat.e2e_mode,
        // Ключ облачного чата получают только участники (чат отдаётся лишь
        // им), поэтому секретность ключа держится на членстве в чате.
        cloud_key: chat.cloud_key.clone(),
        members: members_proto,
        created_at_ms: chat.created_at_ms,
        updated_at_ms: chat.updated_at_ms,
        last_server_id: chat.last_server_id,
        name: chat.name.clone(),
    }
}

/// Готовит `proto::Message` в том виде, в каком его видит конкретное устройство.
///
/// `recipient` — строка получательского ciphertext-а для этого устройства
/// (None — копии нет: отправившее устройство или устройство без E2E).
/// В чатах `E2E_MODE_NONE_DEV` подставляется `plaintext_dev`.
pub fn message_view(
    message: &MessageRecord,
    recipient: Option<&RecipientRecord>,
    e2e_mode: i32,
) -> proto::Message {
    let dev_plaintext = e2e_mode == proto::E2eMode::NoneDev as i32;
    proto::Message {
        server_id: message.server_id,
        chat_id: message.chat_id.clone(),
        sender_user_id: message.sender_user_id.clone(),
        sender_device_id: message.sender_device_id.clone(),
        client_message_id: message.client_message_id.clone(),
        content_kind: message.content_kind.clone(),
        reply_to_server_id: message.reply_to_server_id,
        created_at_ms: message.created_at_ms,
        edited_at_ms: message.edited_at_ms,
        deleted_at_ms: message.deleted_at_ms,
        ciphertext: recipient.map(|r| r.ciphertext.clone()).unwrap_or_default(),
        e2e_version: recipient.map(|r| r.e2e_version).unwrap_or_else(|| {
            // Групповое E2E-сообщение может прийти устройству без персональной
            // копии: ключ сессии ему уже выдавали, содержимое — общий
            // megolm-ciphertext. Режим всё равно E2E.
            if !dev_plaintext && message.group_ciphertext.is_some() {
                rchat_proto::E2E_VERSION_OLM_V1
            } else {
                0
            }
        }),
        plaintext_dev: if dev_plaintext {
            message.plaintext_dev.clone().unwrap_or_default()
        } else {
            String::new()
        },
        // Ключ устройства, создавшего ciphertext: при правке с другого
        // устройства аккаунта он отличается от исходного отправителя.
        sender_curve25519: recipient
            .map(|r| r.sender_curve25519.clone())
            .filter(|key| !key.is_empty())
            .unwrap_or_else(|| message.sender_curve25519.clone()),
        olm_type: recipient.map(|r| r.olm_type).unwrap_or(message.olm_type),
        group_ciphertext: if dev_plaintext {
            Vec::new()
        } else {
            message.group_ciphertext.clone().unwrap_or_default()
        },
        group_session_id: if dev_plaintext {
            String::new()
        } else {
            message.group_session_id.clone()
        },
        recipient_payload_kind: recipient.map(|r| r.payload_kind).unwrap_or(0),
        grouped_id: message.grouped_id.clone(),
        // Облачный режим: содержимое под ключом аккаунта — одинаково для всех
        // устройств аккаунта, поэтому копии на устройство не нужны.
        cloud_ciphertext: message.cloud_ciphertext.clone(),
        cloud_nonce: message.cloud_nonce.clone(),
    }
}

pub fn receipt_event(receipt: &ReceiptRecord) -> sync_event::Payload {
    sync_event::Payload::Receipt(proto::ReceiptEvent {
        chat_id: receipt.chat_id.clone(),
        user_id: receipt.user_id.clone(),
        up_to_server_id: receipt.up_to_server_id,
        read: receipt.read,
    })
}

pub fn chat_created_event(chat: proto::Chat) -> sync_event::Payload {
    sync_event::Payload::ChatCreated(proto::ChatCreatedEvent { chat: Some(chat) })
}

pub fn chat_updated_event(chat: proto::Chat) -> sync_event::Payload {
    sync_event::Payload::ChatUpdated(proto::ChatUpdatedEvent { chat: Some(chat) })
}

/// События запроса на приватный режим.
pub fn private_mode_requested_event(
    chat_id: &str,
    requester_user_id: &str,
    requester_username: &str,
    requester_display_name: &str,
    target_mode: i32,
) -> sync_event::Payload {
    sync_event::Payload::PrivateModeRequested(proto::PrivateModeRequestedEvent {
        chat_id: chat_id.to_string(),
        requester_user_id: requester_user_id.to_string(),
        requester_username: requester_username.to_string(),
        requester_display_name: requester_display_name.to_string(),
        target_mode,
    })
}

pub fn private_mode_cancelled_event(chat_id: &str, requester_user_id: &str) -> sync_event::Payload {
    sync_event::Payload::PrivateModeCancelled(proto::PrivateModeCancelledEvent {
        chat_id: chat_id.to_string(),
        requester_user_id: requester_user_id.to_string(),
    })
}

pub fn private_mode_resolved_event(chat_id: &str, accepted: bool) -> sync_event::Payload {
    sync_event::Payload::PrivateModeResolved(proto::PrivateModeResolvedEvent {
        chat_id: chat_id.to_string(),
        accepted,
    })
}

pub fn message_event(message: proto::Message) -> sync_event::Payload {
    sync_event::Payload::Message(proto::MessageEvent {
        message: Some(message),
    })
}

pub fn message_edited_event(message: proto::Message) -> sync_event::Payload {
    sync_event::Payload::MessageEdited(proto::MessageEditedEvent {
        message: Some(message),
    })
}

pub fn message_deleted_event(message: proto::Message) -> sync_event::Payload {
    sync_event::Payload::MessageDeleted(proto::MessageDeletedEvent {
        message: Some(message),
    })
}

pub fn device_changed_event(
    user_id: &str,
    device_id: &str,
    change: proto::DeviceChangeKind,
    changed_at_ms: u64,
) -> sync_event::Payload {
    sync_event::Payload::DeviceChanged(proto::DeviceChangedEvent {
        user_id: user_id.to_string(),
        device_id: device_id.to_string(),
        change: change as i32,
        changed_at_ms,
    })
}

/// `kind_hint` — номер поля oneof в `SyncEvent` (виден даже старым клиентам).
pub fn kind_hint(payload: &sync_event::Payload) -> u32 {
    match payload {
        sync_event::Payload::ChatCreated(_) => 10,
        sync_event::Payload::Message(_) => 11,
        sync_event::Payload::MessageEdited(_) => 12,
        sync_event::Payload::MessageDeleted(_) => 13,
        sync_event::Payload::Receipt(_) => 14,
        sync_event::Payload::DeviceChanged(_) => 15,
        sync_event::Payload::ChatUpdated(_) => 16,
        sync_event::Payload::PrivateModeRequested(_) => 17,
        sync_event::Payload::PrivateModeCancelled(_) => 18,
        sync_event::Payload::PrivateModeResolved(_) => 19,
    }
}

/// Сериализует событие с `seq = 0` (seq назначит хранилище).
pub fn encode_event_payload(payload: sync_event::Payload) -> Result<Vec<u8>, ServerError> {
    let event = proto::SyncEvent {
        seq: 0,
        kind_hint: kind_hint(&payload),
        payload: Some(payload),
    };
    rchat_proto::encode_message(&event)
        .map_err(|e| ServerError::internal(format!("encode sync event: {e}")))
}

/// Восстанавливает событие из сохранённого payload-а и подставляет seq.
pub fn decode_event_payload(bytes: &[u8], seq: u64) -> Result<proto::SyncEvent, ServerError> {
    let mut event = rchat_proto::decode_message::<proto::SyncEvent>(bytes)
        .map_err(|e| ServerError::internal(format!("decode sync event: {e}")))?;
    event.seq = seq;
    Ok(event)
}

/// Envelope с push-событием (request_id = 0).
pub fn push_sync_event(event: proto::SyncEvent) -> proto::Envelope {
    proto::Envelope {
        request_id: 0,
        body: Some(proto::envelope::Body::Push(proto::Push {
            event: Some(proto::push::Event::SyncEvent(event)),
        })),
    }
}

/// Envelope с transient-событием (presence).
pub fn push_presence(user_id: &str, online: bool, updated_at_ms: u64) -> proto::Envelope {
    proto::Envelope {
        request_id: 0,
        body: Some(proto::envelope::Body::Push(proto::Push {
            event: Some(proto::push::Event::Transient(proto::TransientEvent {
                event: Some(proto::transient_event::Event::Presence(
                    proto::PresenceEvent {
                        user_id: user_id.to_string(),
                        online,
                        updated_at_ms,
                    },
                )),
            })),
        })),
    }
}

pub fn call_to_proto(call: &ActiveCall) -> proto::Call {
    proto::Call {
        call_id: call.call_id.clone(),
        chat_id: call.chat_id.clone(),
        caller_user_id: call.caller_user_id.clone(),
        callee_user_id: call.callee_user_id.clone(),
        state: match call.state {
            CallState::Ringing => proto::CallState::Ringing as i32,
            CallState::Active => proto::CallState::Active as i32,
        },
        created_at_ms: call.created_at_ms,
        accepted_at_ms: call.accepted_at_ms,
    }
}

/// Envelope с transient-событием звонка.
pub fn push_call_event(
    call: &ActiveCall,
    kind: proto::CallEventKind,
    at_ms: u64,
) -> proto::Envelope {
    proto::Envelope {
        request_id: 0,
        body: Some(proto::envelope::Body::Push(proto::Push {
            event: Some(proto::push::Event::Transient(proto::TransientEvent {
                event: Some(proto::transient_event::Event::Call(proto::CallEvent {
                    call: Some(call_to_proto(call)),
                    kind: kind as i32,
                    at_ms,
                })),
            })),
        })),
    }
}

/// Envelope-ответ с ошибкой.
pub fn error_envelope(request_id: u64, error: &ServerError) -> proto::Envelope {
    rchat_proto::error_response(
        request_id,
        rchat_proto::api_error(error.code, error.message.clone(), error.retryable),
    )
}

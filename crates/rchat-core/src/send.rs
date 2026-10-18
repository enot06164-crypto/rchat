//! Сборка исходящего сообщения: выбор получателей, установление E2E-сессий,
//! шифрование. Используется и публичным API (`Client::send_text`), и
//! пересборкой записей outbox внутри актора.

use std::sync::Arc;

use rchat_proto::proto;

use crate::client::Inner;
use crate::error::CoreError;
use crate::traits::PeerDevice;
use crate::types::{ChatRecord, DeviceRecord, LocalAccount};

/// Содержимое исходящего сообщения: ciphertext-ы по устройствам (E2E) либо
/// `plaintext_dev` для явно dev-чата. Открытый текст E2E-чата сюда не попадает.
pub(crate) struct ComposedContent {
    pub content_kind: String,
    pub recipients: Vec<proto::RecipientPayload>,
    pub plaintext_dev: String,
}

/// Компонует содержимое сообщения для чата (используют отправка, правка и
/// пересборка outbox). Для E2E-чата вызывает `fetch_bundles` только если есть
/// устройства без установленной сессии: при наличии сессий сборка возможна
/// офлайн. Открытый текст в E2E-чате никогда не попадает в результат.
pub(crate) async fn compose_content<F, Fut>(
    inner: &Arc<Inner>,
    fetch_bundles: F,
    chat: &ChatRecord,
    account: &LocalAccount,
    text: &str,
) -> Result<ComposedContent, CoreError>
where
    F: FnOnce(Vec<String>) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<PeerDevice>, CoreError>>,
{
    let mut recipients: Vec<proto::RecipientPayload> = Vec::new();
    let mut plaintext_dev = String::new();

    match chat.e2e_mode {
        proto::E2eMode::OlmV1 => {
            let crypto = inner
                .crypto
                .as_ref()
                .ok_or_else(|| {
                    CoreError::E2eUnavailable("e2e chat but no crypto provider configured".into())
                })?
                .clone();
            let members = inner.storage.chat_members(&chat.chat_id).await?;
            if members.is_empty() {
                return Err(CoreError::Internal(format!(
                    "chat {} has no members locally",
                    chat.chat_id
                )));
            }
            let own_device_id = inner.device_id()?;

            // Сбор активных E2E-устройств всех участников, кроме собственного
            // устройства отправителя.
            let mut targets: Vec<PeerDevice> = Vec::new();
            for member in &members {
                let devices = inner.storage.list_devices_of(&member.user_id).await?;
                let usable: Vec<&DeviceRecord> = devices
                    .iter()
                    .filter(|d| {
                        d.revoked_at_ms.is_none()
                            && d.e2e_version == rchat_proto::E2E_VERSION_OLM_V1
                    })
                    .collect();
                if usable.is_empty() {
                    return Err(CoreError::E2eUnavailable(format!(
                        "user {} has no usable e2e devices",
                        member.user_id
                    )));
                }
                for device in usable {
                    if device.user_id == account.user_id && device.device_id == own_device_id {
                        continue;
                    }
                    targets.push(PeerDevice {
                        user_id: device.user_id.clone(),
                        device_id: device.device_id.clone(),
                        ed25519: device.ed25519.clone(),
                        curve25519: device.curve25519.clone(),
                        one_time_key: None,
                    });
                }
            }
            if targets.is_empty() {
                return Err(CoreError::E2eUnavailable(
                    "no recipient devices for e2e message".into(),
                ));
            }

            // Для устройств без сессии требуются prekey с сервера.
            let mut missing_ids: Vec<String> = Vec::new();
            for target in &targets {
                if !crypto.has_session(&target.device_id).await? {
                    missing_ids.push(target.device_id.clone());
                }
            }
            if !missing_ids.is_empty() {
                let bundles = fetch_bundles(missing_ids).await?;
                for target in targets.iter_mut() {
                    let Some(bundle) = bundles.iter().find(|b| b.device_id == target.device_id)
                    else {
                        continue;
                    };
                    // Проверка идентичности: если ключи изменились, сессия не
                    // создаётся — требуется осознанное решение пользователя.
                    if !bundle.curve25519.is_empty()
                        && !target.curve25519.is_empty()
                        && bundle.curve25519 != target.curve25519
                    {
                        return Err(CoreError::Crypto(
                            crate::error::CryptoError::IdentityMismatch(target.device_id.clone()),
                        ));
                    }
                    if !bundle.ed25519.is_empty() {
                        let updated = DeviceRecord {
                            device_id: target.device_id.clone(),
                            user_id: target.user_id.clone(),
                            name: String::new(),
                            platform: String::new(),
                            e2e_version: rchat_proto::E2E_VERSION_OLM_V1,
                            ed25519: bundle.ed25519.clone(),
                            curve25519: bundle.curve25519.clone(),
                            revoked_at_ms: None,
                        };
                        inner.storage.upsert_device(&updated).await?;
                        target.ed25519 = bundle.ed25519.clone();
                        target.curve25519 = bundle.curve25519.clone();
                    }
                    target.one_time_key = bundle.one_time_key.clone();
                }
                // Устройство без пакета ключей получит ошибку MissingPreKey
                // внутри encrypt_for — явная ошибка без fallback на plaintext.
            }

            let encrypted = crypto.encrypt_for(&targets, text.as_bytes()).await?;
            for message in &encrypted {
                recipients.push(proto::RecipientPayload {
                    device_id: message.device_id.clone(),
                    ciphertext: message.ciphertext.clone(),
                    e2e_version: message.e2e_version,
                    olm_type: message.olm_type as i32,
                    sender_curve25519: message.sender_curve25519.clone(),
                    payload_kind: proto::RecipientPayloadKind::Message as i32,
                });
            }
        }
        proto::E2eMode::NoneDev => {
            if inner.config.e2e_enabled {
                // Режим разработки допустим и при включённом E2E, но только
                // в чате, явно созданном как незашифрованный.
                tracing::debug!(
                    chat_id = %chat.chat_id,
                    "sending plaintext in explicitly plaintext dev chat"
                );
            }
            plaintext_dev = text.to_string();
        }
        proto::E2eMode::Unspecified => {
            return Err(CoreError::Protocol(
                "chat has no e2e mode; refusing to send".into(),
            ));
        }
    }

    Ok(ComposedContent {
        content_kind: "text".to_string(),
        recipients,
        plaintext_dev,
    })
}

/// Компонует `SendMessageRequest` для чата.
pub(crate) async fn compose_send_request<F, Fut>(
    inner: &Arc<Inner>,
    local_id: &str,
    fetch_bundles: F,
    chat: &ChatRecord,
    account: &LocalAccount,
    text: &str,
    reply_to: Option<u64>,
) -> Result<proto::SendMessageRequest, CoreError>
where
    F: FnOnce(Vec<String>) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<PeerDevice>, CoreError>>,
{
    let content = compose_content(inner, fetch_bundles, chat, account, text).await?;
    let request = proto::SendMessageRequest {
        chat_id: chat.chat_id.clone(),
        client_message_id: local_id.to_string(),
        content_kind: content.content_kind,
        reply_to_server_id: reply_to.unwrap_or(0),
        recipients: content.recipients,
        plaintext_dev: content.plaintext_dev,
        group_ciphertext: Vec::new(),
        group_session_id: String::new(),
    };
    check_request_size(inner, &request)?;
    Ok(request)
}

/// Компонует `EditMessageRequest` (правка текста автором): то же шифрование,
/// что и при отправке, но без `client_message_id`.
pub(crate) async fn compose_edit_request<F, Fut>(
    inner: &Arc<Inner>,
    fetch_bundles: F,
    chat: &ChatRecord,
    account: &LocalAccount,
    server_id: u64,
    text: &str,
) -> Result<proto::EditMessageRequest, CoreError>
where
    F: FnOnce(Vec<String>) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<PeerDevice>, CoreError>>,
{
    let content = compose_content(inner, fetch_bundles, chat, account, text).await?;
    let request = proto::EditMessageRequest {
        server_id,
        content_kind: content.content_kind,
        recipients: content.recipients,
        plaintext_dev: content.plaintext_dev,
        group_ciphertext: Vec::new(),
        group_session_id: String::new(),
    };
    check_request_size(inner, &request)?;
    Ok(request)
}

/// Проверяет, что закодированный запрос укладывается в лимит сервера.
fn check_request_size<M: prost::Message>(inner: &Inner, request: &M) -> Result<(), CoreError> {
    let max_message_bytes = inner
        .server_hello
        .read()
        .expect("hello lock poisoned")
        .as_ref()
        .map(|h| h.max_message_bytes)
        .filter(|v| *v > 0)
        .unwrap_or(rchat_proto::limits::MAX_MESSAGE_BYTES_DEFAULT)
        as usize;
    let size = prost::Message::encoded_len(request);
    if size > max_message_bytes {
        return Err(CoreError::InvalidInput(format!(
            "message too large: {size} > {max_message_bytes} bytes"
        )));
    }
    Ok(())
}

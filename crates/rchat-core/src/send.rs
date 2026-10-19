//! Сборка исходящего сообщения: выбор получателей, установление E2E-сессий,
//! шифрование. Используется и публичным API (`Client::send_text`), и
//! пересборкой записей outbox внутри актора.

use std::sync::Arc;

use rchat_proto::proto;

use crate::client::Inner;
use crate::error::{CoreError, CryptoError};
use crate::payload::Payload;
use crate::traits::PeerDevice;
use crate::types::{ChatRecord, DeviceRecord, LocalAccount};

/// Содержимое исходящего сообщения: ciphertext-ы по устройствам (E2E) либо
/// `plaintext_dev` для явно dev-чата. Открытый текст E2E-чата сюда не попадает.
pub(crate) struct ComposedContent {
    pub content_kind: String,
    pub recipients: Vec<proto::RecipientPayload>,
    pub plaintext_dev: String,
    /// Общий megolm-ciphertext группового сообщения (пусто для личных чатов).
    pub group_ciphertext: Vec<u8>,
    /// Идентификатор megolm-сессии (пусто для личных чатов).
    pub group_session_id: String,
    /// Устройства, которым в этом запросе выдан ключ групповой сессии:
    /// фиксируются в хранилище после постановки запроса в outbox, чтобы
    /// не рассылать ключ повторно и досылать его новым устройствам.
    pub group_key_shares: Vec<String>,
}

/// Дозаполняет дескрипторы устройств свежими пакетами ключей с сервера:
/// сверяет identity key (несовпадение — явная ошибка без fallback) и
/// подставляет prekey для установления сессии.
async fn apply_bundles(
    inner: &Arc<Inner>,
    targets: &mut [PeerDevice],
    bundles: &[PeerDevice],
) -> Result<(), CoreError> {
    for target in targets.iter_mut() {
        let Some(bundle) = bundles.iter().find(|b| b.device_id == target.device_id) else {
            continue;
        };
        if !bundle.curve25519.is_empty()
            && !target.curve25519.is_empty()
            && bundle.curve25519 != target.curve25519
        {
            return Err(CoreError::Crypto(CryptoError::IdentityMismatch(
                target.device_id.clone(),
            )));
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
    Ok(())
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
    payload: &Payload,
) -> Result<ComposedContent, CoreError>
where
    F: FnOnce(Vec<String>) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<PeerDevice>, CoreError>>,
{
    let mut recipients: Vec<proto::RecipientPayload> = Vec::new();
    let mut plaintext_dev = String::new();
    // Конверт (текст + вложение) шифруется целиком: байты файла не видны
    // серверу, как и текст.
    let payload_bytes = payload.encode();
    let content_kind = payload.content_kind();

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
            let is_group = chat.kind == proto::ChatKind::Group;

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
                    if is_group {
                        // В группе «нет устройств у участника» — не ошибка:
                        // ключ просто некому выдать, он получит его позже
                        // (при следующем входе/устройстве).
                        tracing::warn!(
                            user_id = %member.user_id,
                            chat_id = %chat.chat_id,
                            "group member without usable e2e devices"
                        );
                        continue;
                    }
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

            if is_group {
                // Группа: один общий megolm-ciphertext; recipients несут
                // только ключ сессии тем устройствам, кому он ещё не выдан.
                let encryption = crypto
                    .group_encrypt(&chat.chat_id, &payload_bytes, true)
                    .await?;
                let already = inner
                    .storage
                    .group_key_shares(&chat.chat_id, &encryption.session_id)
                    .await?;
                let mut needed: Vec<PeerDevice> = targets
                    .into_iter()
                    .filter(|t| !already.contains(&t.device_id))
                    .collect();
                let mut group_key_shares: Vec<String> = Vec::new();
                if !needed.is_empty() {
                    if let Some(session_key) = encryption.session_key.as_ref() {
                        // Prekey нужен только тем, с кем ещё нет olm-сессии;
                        // остальным ключ летит обычным olm-сообщением.
                        let mut missing_ids: Vec<String> = Vec::new();
                        for target in &needed {
                            if !crypto.has_session(&target.device_id).await? {
                                missing_ids.push(target.device_id.clone());
                            }
                        }
                        if !missing_ids.is_empty() {
                            let bundles = fetch_bundles(missing_ids).await?;
                            apply_bundles(inner, &mut needed, &bundles).await?;
                        }
                        let envelopes = crypto
                            .encrypt_group_key(&chat.chat_id, session_key, &needed)
                            .await?;
                        for message in &envelopes {
                            recipients.push(proto::RecipientPayload {
                                device_id: message.device_id.clone(),
                                ciphertext: message.ciphertext.clone(),
                                e2e_version: message.e2e_version,
                                olm_type: message.olm_type as i32,
                                sender_curve25519: message.sender_curve25519.clone(),
                                payload_kind: proto::RecipientPayloadKind::GroupKey as i32,
                            });
                            group_key_shares.push(message.device_id.clone());
                        }
                    }
                }
                return Ok(ComposedContent {
                    content_kind,
                    recipients,
                    plaintext_dev,
                    group_ciphertext: encryption.ciphertext,
                    group_session_id: encryption.session_id,
                    group_key_shares,
                });
            }

            // Личный E2E-чат: каждый участник обязан иметь пригодные
            // устройства, копия шифруется персонально каждому.
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
                apply_bundles(inner, &mut targets, &bundles).await?;
                // Устройство без пакета ключей получит ошибку MissingPreKey
                // внутри encrypt_for — явная ошибка без fallback на plaintext.
            }

            let encrypted = crypto.encrypt_for(&targets, &payload_bytes).await?;
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
            plaintext_dev = String::from_utf8_lossy(&payload_bytes).into_owned();
        }
        proto::E2eMode::Unspecified => {
            return Err(CoreError::Protocol(
                "chat has no e2e mode; refusing to send".into(),
            ));
        }
    }

    Ok(ComposedContent {
        content_kind,
        recipients,
        plaintext_dev,
        group_ciphertext: Vec::new(),
        group_session_id: String::new(),
        group_key_shares: Vec::new(),
    })
}

/// Параметры отправки, не относящиеся к содержимому.
pub(crate) struct SendOptions {
    /// Ответ на сообщение с этим `server_id`.
    pub reply_to: Option<u64>,
    /// Идентификатор альбома (медиагруппы); пусто для обычных сообщений.
    pub grouped_id: Option<String>,
}

/// Компонует `SendMessageRequest` для чата. Возвращает запрос и список
/// устройств, которым в этом запросе выдан ключ групповой сессии (для
/// фиксации в хранилище после постановки в outbox).
pub(crate) async fn compose_send_request<F, Fut>(
    inner: &Arc<Inner>,
    local_id: &str,
    fetch_bundles: F,
    chat: &ChatRecord,
    account: &LocalAccount,
    payload: &Payload,
    options: &SendOptions,
) -> Result<(proto::SendMessageRequest, Vec<String>), CoreError>
where
    F: FnOnce(Vec<String>) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<PeerDevice>, CoreError>>,
{
    let content = compose_content(inner, fetch_bundles, chat, account, payload).await?;
    let request = proto::SendMessageRequest {
        chat_id: chat.chat_id.clone(),
        client_message_id: local_id.to_string(),
        content_kind: content.content_kind,
        reply_to_server_id: options.reply_to.unwrap_or(0),
        recipients: content.recipients,
        plaintext_dev: content.plaintext_dev,
        group_ciphertext: content.group_ciphertext,
        group_session_id: content.group_session_id,
        grouped_id: options.grouped_id.clone().unwrap_or_default(),
    };
    check_request_size(inner, &request)?;
    Ok((request, content.group_key_shares))
}

/// Компонует `EditMessageRequest` (правка текста автором): то же шифрование,
/// что и при отправке, но без `client_message_id`.
pub(crate) async fn compose_edit_request<F, Fut>(
    inner: &Arc<Inner>,
    fetch_bundles: F,
    chat: &ChatRecord,
    account: &LocalAccount,
    server_id: u64,
    payload: &Payload,
) -> Result<(proto::EditMessageRequest, Vec<String>), CoreError>
where
    F: FnOnce(Vec<String>) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<PeerDevice>, CoreError>>,
{
    let content = compose_content(inner, fetch_bundles, chat, account, payload).await?;
    let request = proto::EditMessageRequest {
        server_id,
        content_kind: content.content_kind,
        recipients: content.recipients,
        plaintext_dev: content.plaintext_dev,
        group_ciphertext: content.group_ciphertext,
        group_session_id: content.group_session_id,
    };
    check_request_size(inner, &request)?;
    Ok((request, content.group_key_shares))
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

//! Диспетчеризация JSON-запросов.
//!
//! Каждый запрос — объект с полем `@type`. Ответ — `{"@type":"ok", ...}` или
//! `{"@type":"error", ...}`; события приходят через `rchat_receive_json`.
//! Полная схема — `docs/ffi.md`.

use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use rchat_core::payload::{Attachment, AttachmentKind, BlobRef};
use rchat_core::traits::Storage;
use rchat_core::types::ChatMode;
use serde_json::{json, Value};

use crate::error::FfiError;
use crate::json;
use crate::RchatHandle;

/// Версия JSON-схемы. Изменяется только при несовместимом изменении
/// формата запросов/ответов/событий.
pub(crate) const API_VERSION: u32 = 1;

pub(crate) async fn dispatch(
    handle: &RchatHandle,
    method: &str,
    request: &Value,
) -> Result<Value, FfiError> {
    match method {
        "ping" => Ok(json!({
            "service": "rchat-ffi",
            "version": env!("CARGO_PKG_VERSION"),
            "api_version": API_VERSION,
        })),
        "start" => {
            handle.client.start().await?;
            Ok(connection_state_value(handle))
        }
        "get_connection_state" => Ok(connection_state_value(handle)),
        "wait_online" => {
            let timeout = timeout_ms(request, "timeout_ms", 5_000)?;
            if timeout > 0 {
                handle.client.start().await?;
                handle
                    .client
                    .wait_online(Duration::from_millis(timeout))
                    .await?;
            }
            Ok(connection_state_value(handle))
        }
        "get_session" => session_value(handle).await,
        "get_account" => account_value(handle).await,
        "get_status" => status_value(handle).await,
        "register" => {
            let username = string_field(request, "username")?;
            let display_name = optional_string_field(request, "display_name")?
                .unwrap_or_else(|| username.to_string());
            let password = string_field(request, "password")?;
            handle
                .client
                .register(&username, &display_name, &password)
                .await?;
            session_value(handle).await
        }
        "login" => {
            let username = string_field(request, "username")?;
            let password = string_field(request, "password")?;
            handle.client.login(&username, &password).await?;
            session_value(handle).await
        }
        "logout" => {
            let wipe = bool_field(request, "wipe", false)?;
            handle.client.logout(wipe).await?;
            Ok(json!({"wiped": wipe}))
        }
        "get_settings" => {
            let mode = handle.client.default_chat_mode();
            Ok(json!({
                "default_chat_mode": mode.as_str(),
                "default_chat_privacy": match mode {
                    ChatMode::Cloud => "cloud",
                    _ => "private",
                },
            }))
        }
        "set_default_chat_mode" => {
            let mode = string_field(request, "mode")?;
            let mode = ChatMode::parse(mode.trim()).ok_or_else(|| {
                FfiError::invalid(format!(
                    "unknown mode `{mode}`: expected cloud, private or none"
                ))
            })?;
            handle.client.set_default_chat_mode(mode).await?;
            Ok(json!({"default_chat_mode": mode.as_str()}))
        }
        "search_messages" => {
            let query = string_field(request, "query")?;
            let limit = optional_u64_field(request, "limit")?.unwrap_or(50) as u32;
            let hits = handle.client.search_messages(&query, limit).await?;
            Ok(json!({
                "hits": hits
                    .iter()
                    .map(|hit| json!({
                        "chat_id": hit.chat_id,
                        "server_id": hit.server_id,
                        "body": hit.body,
                        "created_at_ms": hit.created_at_ms,
                        "chat_title": hit.chat_title,
                    }))
                    .collect::<Vec<_>>(),
            }))
        }
        "get_chats" => {
            let chats = handle.storage.list_chats().await?;
            Ok(Value::Array(chats.iter().map(json::chat).collect()))
        }
        "refresh_chats" => {
            let chats = handle.client.list_chats().await?;
            Ok(Value::Array(chats.iter().map(json::chat).collect()))
        }
        "get_chat_members" => {
            let chat_id = string_field(request, "chat_id")?;
            let members = handle.client.chat_members(&chat_id).await?;
            Ok(Value::Array(members.iter().map(json::member).collect()))
        }
        "create_direct_chat" => {
            let peer = string_field(request, "peer_username")?;
            let mode = chat_mode_field(request, handle.client.default_chat_mode())?;
            let chat = handle.client.create_direct_chat(&peer, mode).await?;
            Ok(json::chat(&chat))
        }
        "create_group_chat" => {
            let name = string_field(request, "name")?;
            let members = string_list(request, "members", false)?;
            let mode = chat_mode_field(request, handle.client.default_chat_mode())?;
            let chat = handle
                .client
                .create_group_chat(&name, &members, mode)
                .await?;
            Ok(json::chat(&chat))
        }
        "invite_to_chat" => {
            let chat_id = string_field(request, "chat_id")?;
            let usernames = string_list(request, "usernames", true)?;
            let chat = handle.client.invite_to_chat(&chat_id, &usernames).await?;
            Ok(json::chat(&chat))
        }
        "leave_chat" => {
            let chat_id = string_field(request, "chat_id")?;
            handle.client.leave_chat(&chat_id).await?;
            Ok(Value::Null)
        }
        "remove_chat_member" => {
            let chat_id = string_field(request, "chat_id")?;
            let user_id = string_field(request, "user_id")?;
            let chat = handle.client.remove_chat_member(&chat_id, &user_id).await?;
            Ok(json::chat(&chat))
        }
        "rename_chat" => {
            let chat_id = string_field(request, "chat_id")?;
            let name = string_field(request, "name")?;
            let chat = handle.client.rename_chat(&chat_id, &name).await?;
            Ok(json::chat(&chat))
        }
        "send_message" => {
            let chat_id = string_field(request, "chat_id")?;
            let reply_to = optional_u64_field(request, "reply_to_server_id")?;
            // Текст может отсутствовать, если есть вложение; вложение — опционально.
            let text = match request.get("text") {
                Some(Value::String(value)) => value.clone(),
                Some(Value::Null) | None => String::new(),
                Some(_) => return Err(FfiError::invalid("field text must be a string")),
            };
            // Вложения: одно или несколько (альбом).
            let mut attachments: Vec<Attachment> = Vec::new();
            if let Some(items) = request.get("attachments").and_then(Value::as_array) {
                for item in items {
                    attachments.push(parse_attachment(item)?);
                }
            }
            if let Some(item) = request.get("attachment") {
                if !item.is_null() {
                    attachments.push(parse_attachment(item)?);
                }
            }
            if text.trim().is_empty() && attachments.is_empty() {
                return Err(FfiError::invalid("message must have text or attachment"));
            }
            if attachments.len() > 1 {
                // Альбом: несколько вложений одним набором (общий grouped_id).
                let local_ids = handle
                    .client
                    .send_album(&chat_id, &text, attachments, reply_to)
                    .await?;
                return Ok(json!({"local_ids": local_ids}));
            }
            let local_id = match attachments.into_iter().next() {
                Some(item) => {
                    handle
                        .client
                        .send_attachment(&chat_id, &text, item, reply_to)
                        .await?
                }
                None => handle.client.send_text(&chat_id, &text, reply_to).await?,
            };
            Ok(json!({"local_id": local_id}))
        }
        "retry_message" => {
            // Повтор отправки: та же запись очереди (тот же ключ
            // идемпотентности) — новой копии сообщения не создаётся.
            let local_id = string_field(request, "local_id")?;
            let requeued = handle.client.retry_message(&local_id).await?;
            Ok(json!({"requeued": requeued}))
        }
        "download_attachment" => {
            let server_id = u64_field(request, "server_id")?;
            // Ядро само скачивает блоб с сервера и расшифровывает его ключом
            // из сообщения (файл едет отдельным блобом, как в Telegram).
            let attachment = handle
                .client
                .attachment_content(server_id)
                .await?
                .ok_or_else(|| {
                    FfiError::new(
                        "not_found",
                        format!("message {server_id} has no attachment"),
                    )
                })?;
            // Постер видео (если есть) — отдельным запросом к ядру.
            let thumb = handle
                .client
                .attachment_thumb_content(server_id)
                .await?
                .map(|bytes| BASE64.encode(&bytes));
            Ok(json!({
                "kind": attachment.kind.as_str(),
                "name": attachment.name,
                "mime": attachment.mime,
                "size": attachment.size,
                "data_base64": BASE64.encode(&attachment.data),
                "thumb_base64": thumb,
            }))
        }
        "get_messages" => {
            let chat_id = string_field(request, "chat_id")?;
            let before = optional_u64_field(request, "before_server_id")?;
            let limit = limit_field(request)?;
            let messages = handle.client.list_messages(&chat_id, before, limit).await?;
            Ok(Value::Array(messages.iter().map(json::message).collect()))
        }
        "fetch_history" => {
            let chat_id = string_field(request, "chat_id")?;
            let before = optional_u64_field(request, "before_server_id")?;
            let limit = limit_field(request)?;
            let messages = handle.client.fetch_history(&chat_id, before, limit).await?;
            Ok(Value::Array(messages.iter().map(json::message).collect()))
        }
        "edit_message" => {
            let chat_id = string_field(request, "chat_id")?;
            let server_id = u64_field(request, "server_id")?;
            let text = text_field(request, "text")?;
            let message = handle.client.edit_text(&chat_id, server_id, &text).await?;
            Ok(json::message(&message))
        }
        "delete_message" => {
            let chat_id = string_field(request, "chat_id")?;
            let server_id = u64_field(request, "server_id")?;
            let message = handle.client.delete_message(&chat_id, server_id).await?;
            Ok(json::message(&message))
        }
        "mark_read" => {
            let chat_id = string_field(request, "chat_id")?;
            let up_to = match optional_u64_field(request, "up_to_server_id")? {
                Some(value) => value,
                None => handle
                    .storage
                    .get_chat(&chat_id)
                    .await?
                    .map(|chat| chat.last_server_id)
                    .unwrap_or(0),
            };
            if up_to == 0 {
                return Ok(json!({"up_to_server_id": 0}));
            }
            handle.client.mark_read(&chat_id, up_to).await?;
            Ok(json!({"up_to_server_id": up_to}))
        }
        "get_receipts" => {
            let chat_id = string_field(request, "chat_id")?;
            let receipts = handle.client.receipts(&chat_id).await?;
            Ok(Value::Array(receipts.iter().map(json::receipt).collect()))
        }
        "sync" => {
            let progress = handle.client.sync_now().await?;
            Ok(json::progress(progress))
        }
        "get_devices" => {
            let devices = handle.client.devices().await?;
            Ok(Value::Array(devices.iter().map(json::device).collect()))
        }
        "refresh_devices" => {
            let user_ids = string_list(request, "user_ids", true)?;
            handle.client.refresh_devices(&user_ids).await?;
            Ok(Value::Null)
        }
        "revoke_device" => {
            let device_id = string_field(request, "device_id")?;
            handle.client.revoke_device(&device_id).await?;
            Ok(Value::Null)
        }
        "call_invite" => {
            let chat_id = string_field(request, "chat_id")?;
            let call = handle.client.call_invite(&chat_id).await?;
            Ok(json::call(&call))
        }
        "call_accept" => {
            let call_id = string_field(request, "call_id")?;
            let call = handle.client.call_accept(&call_id).await?;
            Ok(json::call(&call))
        }
        "call_decline" => {
            let call_id = string_field(request, "call_id")?;
            let call = handle.client.call_decline(&call_id).await?;
            Ok(json::call(&call))
        }
        "call_end" => {
            let call_id = string_field(request, "call_id")?;
            let call = handle.client.call_end(&call_id).await?;
            Ok(json::call(&call))
        }
        other => Err(FfiError::unknown_method(other)),
    }
}

fn connection_state_value(handle: &RchatHandle) -> Value {
    json!({"state": json::connection_state(handle.client.connection_state())})
}

async fn session_value(handle: &RchatHandle) -> Result<Value, FfiError> {
    let account = handle.storage.load_account().await?;
    let active = handle.client.session();
    let value = match (account, active) {
        (Some(account), Some((user_id, username, display_name))) => session_object(
            true,
            &user_id,
            &username,
            &display_name,
            json!(account.device_id),
        ),
        (Some(account), None) => session_object(
            false,
            &account.user_id,
            &account.username,
            &account.display_name,
            json!(account.device_id),
        ),
        (None, Some((user_id, username, display_name))) => {
            session_object(true, &user_id, &username, &display_name, Value::Null)
        }
        (None, None) => Value::Null,
    };
    Ok(value)
}

fn session_object(
    active: bool,
    user_id: &str,
    username: &str,
    display_name: &str,
    device_id: Value,
) -> Value {
    json!({
        "active": active,
        "user_id": user_id,
        "username": username,
        "display_name": display_name,
        "device_id": device_id,
        "avatar": json::avatar(user_id, display_name, username),
    })
}

async fn account_value(handle: &RchatHandle) -> Result<Value, FfiError> {
    Ok(match handle.storage.load_account().await? {
        Some(account) => json::account(&account),
        None => Value::Null,
    })
}

async fn status_value(handle: &RchatHandle) -> Result<Value, FfiError> {
    let (total, due) = handle.storage.outbox_stats(rchat_proto::now_ms()).await?;
    Ok(json!({
        "connection": json::connection_state(handle.client.connection_state()),
        "session": session_value(handle).await?,
        "account": account_value(handle).await?,
        "sync_cursor": handle.storage.get_sync_cursor().await?,
        "outbox": {"total": total, "due": due},
        "data_dir": handle.data_dir.display().to_string(),
        "server": handle.server,
        "device_name": handle.device_name,
        "e2e": handle.e2e,
    }))
}

fn string_field(request: &Value, name: &str) -> Result<String, FfiError> {
    match request.get(name) {
        Some(Value::String(value)) => Ok(value.clone()),
        Some(_) => Err(FfiError::invalid(format!("field {name} must be a string"))),
        None => Err(FfiError::invalid(format!("missing field {name}"))),
    }
}

/// Текстовое поле сообщения: пустые и пробельные строки отклоняются до
/// шифрования/постановки в очередь.
fn text_field(request: &Value, name: &str) -> Result<String, FfiError> {
    let text = string_field(request, name)?;
    if text.trim().is_empty() {
        return Err(FfiError::invalid(format!("field {name} must not be empty")));
    }
    Ok(text)
}

/// Разбор поля `attachment`: {kind, name, mime, size, data_base64}.
fn parse_attachment(item: &Value) -> Result<Attachment, FfiError> {
    let kind_raw = item
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| FfiError::invalid("attachment.kind must be a string"))?;
    let kind = AttachmentKind::parse(kind_raw)
        .ok_or_else(|| FfiError::invalid(format!("attachment.kind {kind_raw} is not supported")))?;
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mime = item
        .get("mime")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let data = match item.get("data_base64").and_then(Value::as_str) {
        Some(raw) => BASE64
            .decode(raw)
            .map_err(|e| FfiError::invalid(format!("attachment.data_base64: {e}")))?,
        None => Vec::new(),
    };
    if data.is_empty() {
        return Err(FfiError::invalid(
            "attachment.data_base64 must not be empty",
        ));
    }
    let size = item
        .get("size")
        .and_then(Value::as_u64)
        .unwrap_or(data.len() as u64);
    // Постер видео (отдельный блоб) и пики звуковой волны.
    let thumb = match item.get("thumb_base64").and_then(Value::as_str) {
        Some(raw) => {
            let bytes = BASE64
                .decode(raw)
                .map_err(|e| FfiError::invalid(format!("attachment.thumb_base64: {e}")))?;
            if bytes.is_empty() {
                None
            } else {
                Some(BlobRef {
                    reference: String::new(),
                    key: Vec::new(),
                    nonce: Vec::new(),
                    size: bytes.len() as u64,
                    mime: item
                        .get("thumb_mime")
                        .and_then(Value::as_str)
                        .unwrap_or("image/jpeg")
                        .to_string(),
                    data: bytes,
                })
            }
        }
        None => None,
    };
    let waveform = match item.get("waveform_base64").and_then(Value::as_str) {
        Some(raw) => BASE64
            .decode(raw)
            .map_err(|e| FfiError::invalid(format!("attachment.waveform_base64: {e}")))?,
        None => Vec::new(),
    };
    let attachment = Attachment {
        kind,
        name,
        mime,
        size,
        blob_ref: None,
        key: None,
        nonce: None,
        thumb,
        waveform,
        data,
    };
    attachment.validate().map_err(FfiError::invalid)?;
    Ok(attachment)
}

fn optional_string_field(request: &Value, name: &str) -> Result<Option<String>, FfiError> {
    match request.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(FfiError::invalid(format!("field {name} must be a string"))),
    }
}

fn u64_field(request: &Value, name: &str) -> Result<u64, FfiError> {
    match request.get(name).and_then(Value::as_u64) {
        Some(value) => Ok(value),
        None => Err(FfiError::invalid(format!(
            "field {name} must be a non-negative integer"
        ))),
    }
}

fn optional_u64_field(request: &Value, name: &str) -> Result<Option<u64>, FfiError> {
    match request.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number.as_u64().map(Some).ok_or_else(|| {
            FfiError::invalid(format!("field {name} must be a non-negative integer"))
        }),
        Some(_) => Err(FfiError::invalid(format!(
            "field {name} must be a non-negative integer"
        ))),
    }
}

/// Режим чата из запроса: поле `mode` (`cloud` / `private` / `none`).
/// Для совместимости поддерживается и прежнее поле `e2e`: `true` — приватный
/// чат, `false` — чат без шифрования (режим разработки). Если поля нет —
/// берётся режим по умолчанию из настроек клиента.
fn chat_mode_field(request: &Value, default: ChatMode) -> Result<ChatMode, FfiError> {
    match request.get("mode") {
        None | Some(Value::Null) => {}
        Some(Value::String(value)) => {
            return ChatMode::parse(value.trim()).ok_or_else(|| {
                FfiError::invalid(format!(
                    "unknown mode `{value}`: expected cloud, private or none"
                ))
            })
        }
        Some(_) => return Err(FfiError::invalid("field mode must be a string")),
    }
    match request.get("e2e") {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(true)) => Ok(ChatMode::Private),
        Some(Value::Bool(false)) => Ok(ChatMode::NoneDev),
        Some(_) => Err(FfiError::invalid("field e2e must be a boolean")),
    }
}

fn bool_field(request: &Value, name: &str, default: bool) -> Result<bool, FfiError> {
    match request.get(name) {
        None => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(FfiError::invalid(format!("field {name} must be a boolean"))),
    }
}

fn string_list(request: &Value, name: &str, required: bool) -> Result<Vec<String>, FfiError> {
    let value = match request.get(name) {
        Some(value) => value,
        None if required => {
            return Err(FfiError::invalid(format!("missing field {name}")));
        }
        None => return Ok(Vec::new()),
    };
    let items = value
        .as_array()
        .ok_or_else(|| FfiError::invalid(format!("field {name} must be an array of strings")))?;
    items
        .iter()
        .map(|item| {
            item.as_str().map(str::to_string).ok_or_else(|| {
                FfiError::invalid(format!("field {name} must be an array of strings"))
            })
        })
        .collect()
}

fn limit_field(request: &Value) -> Result<u32, FfiError> {
    let limit = optional_u64_field(request, "limit")?.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(FfiError::invalid("field limit must be in 1..=100"));
    }
    Ok(limit as u32)
}

fn timeout_ms(request: &Value, name: &str, default: u64) -> Result<u64, FfiError> {
    let value = optional_u64_field(request, name)?.unwrap_or(default);
    if value > 600_000 {
        return Err(FfiError::invalid(format!(
            "field {name} must be in 0..=600000"
        )));
    }
    Ok(value)
}

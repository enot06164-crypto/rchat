//! Актор соединения ядра: handshake, аутентификация, синхронизация, outbox,
//! обработка push-событий. Один экземпляр актора обслуживает одно соединение
//! (с автоматическим реконнектом) и последовательно обрабатывает команды API,
//! сетевые кадры и тики таймера.

use std::sync::Arc;
use std::time::Duration;

use rchat_proto::{envelope, proto, push, request, response, sync_event, transient_event};
use tokio::sync::mpsc;
use tokio::time::{interval, timeout, MissedTickBehavior};
use tracing::{debug, info, warn};

use crate::backoff::{delay_for_attempt, Backoff};
use crate::client::{Command, Inner, LoopState, SessionInfo};
use crate::convert;
use crate::error::{rejected_from, CoreError};
use crate::events::UiEvent;
use crate::traits::{secret_keys, PeerDevice, Transport};
use crate::types::{
    ConnectionState, LocalAccount, MessageRecord, MessageStatus, OutboxState, ReceiptRecord,
    SyncApply, SyncProgress,
};

/// Прекращение сессии с указанием причины.
enum SessionError {
    /// Несовместимость протокола/конфигурации — повторять нет смысла.
    Fatal(CoreError),
    /// Транспортная или временная ошибка — переподключиться с backoff.
    Retry(CoreError),
}

impl From<CoreError> for SessionError {
    fn from(error: CoreError) -> Self {
        SessionError::Retry(error)
    }
}

fn is_transport_error(error: &CoreError) -> bool {
    matches!(error, CoreError::Transport(_))
}

/// Нарушение протокола (включая поток битых кадров): соединение закрывается,
/// клиент переподключается с чистым потоком вместо бесконечного терпения.
fn is_protocol_error(error: &CoreError) -> bool {
    matches!(error, CoreError::Protocol(_))
}

// ---------------------------------------------------------------------------
// Точка входа актора
// ---------------------------------------------------------------------------

pub(crate) async fn actor_main(inner: Arc<Inner>, mut commands: mpsc::Receiver<Command>) {
    let mut backoff = Backoff::new(inner.config.backoff);
    let mut first_attempt = true;
    loop {
        if inner.is_shutdown() {
            break;
        }
        inner.set_state(if first_attempt {
            ConnectionState::Connecting
        } else {
            ConnectionState::Reconnecting
        });
        first_attempt = false;
        match session_loop(&inner, &mut commands).await {
            Ok(()) => break, // штатное завершение
            Err(SessionError::Fatal(error)) => {
                warn!(error = %error, "fatal session error, stopping reconnects");
                inner.emit(UiEvent::SessionEnded {
                    reason: error.to_string(),
                });
                break;
            }
            Err(SessionError::Retry(error)) => {
                if is_transport_error(&error) {
                    debug!(error = %error, "connection lost");
                } else {
                    warn!(error = %error, "session error");
                }
                inner.set_session(None);
                inner.set_state(ConnectionState::Disconnected);
                let delay = backoff.next_delay();
                if wait_or_shutdown(&inner, &mut commands, delay).await {
                    break;
                }
            }
        }
    }
    inner.set_state(ConnectionState::Disconnected);
}

/// Ждёт задержку, параллельно отвечая на команды "нет соединения".
/// Возвращает true, если получен Shutdown.
async fn wait_or_shutdown(
    inner: &Arc<Inner>,
    commands: &mut mpsc::Receiver<Command>,
    delay: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + delay;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return false,
            cmd = commands.recv() => match cmd {
                None => return true,
                Some(Command::Shutdown { reply }) => {
                    let _ = reply.send(());
                    return true;
                }
                Some(other) => reply_offline(inner, other).await,
            },
        }
    }
}

async fn reply_offline(inner: &Arc<Inner>, command: Command) {
    match command {
        Command::Request { reply, .. } => {
            let _ = reply.send(Err(CoreError::NotConnected));
        }
        Command::Register { reply, .. } | Command::Login { reply, .. } => {
            let _ = reply.send(Err(CoreError::NotConnected));
        }
        Command::SyncNow { reply } => {
            let _ = reply.send(Err(CoreError::NotConnected));
        }
        Command::FetchHistory { reply, .. } => {
            let _ = reply.send(Err(CoreError::NotConnected));
        }
        Command::Logout { wipe, reply } => {
            let result = logout_local(inner, wipe).await;
            let _ = reply.send(result);
        }
        Command::Kick | Command::Shutdown { .. } => {}
    }
}

async fn logout_local(inner: &Arc<Inner>, wipe: bool) -> Result<(), CoreError> {
    let _ = inner.secrets.delete(secret_keys::SESSION_TOKEN);
    if wipe {
        inner.storage.clear_all().await?;
        if let Some(crypto) = inner.crypto.as_ref() {
            crypto.wipe().await?;
        }
        let _ = inner.secrets.delete(secret_keys::CRYPTO_PICKLE_KEY);
        let _ = inner.secrets.delete(secret_keys::DEVICE_ID);
        inner.clear_device_id_cache();
    }
    inner.set_session(None);
    inner.set_state(ConnectionState::Unauthenticated);
    inner.emit(UiEvent::SessionEnded {
        reason: "logout".to_string(),
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Сессия соединения
// ---------------------------------------------------------------------------

enum Wake {
    Command(Option<Command>),
    Frame(Result<Option<Vec<u8>>, crate::error::TransportError>),
    Tick,
}

async fn session_loop(
    inner: &Arc<Inner>,
    commands: &mut mpsc::Receiver<Command>,
) -> Result<(), SessionError> {
    let mut transport = match timeout(inner.config.connect_timeout, inner.transport.connect()).await
    {
        Err(_) => {
            return Err(SessionError::Retry(CoreError::Transport(
                crate::error::TransportError::new(
                    crate::error::TransportErrorKind::Connect,
                    "connect timeout",
                ),
            )))
        }
        Ok(Err(error)) => return Err(SessionError::Retry(CoreError::Transport(error))),
        Ok(Ok(transport)) => transport,
    };

    let server_hello = handshake(inner, &mut transport).await?;
    *inner.server_hello.write().expect("hello lock poisoned") = Some(server_hello);

    let mut state = LoopState::new();
    let mut ctx = Ctx {
        inner,
        transport: &mut transport,
        st: &mut state,
    };

    // Пробуем восстановить сохранённую сессию.
    if let Some(token) = ctx
        .inner
        .secrets
        .get(secret_keys::SESSION_TOKEN)
        .ok()
        .flatten()
        .and_then(|bytes| String::from_utf8(bytes).ok())
    {
        match ctx.resume(&token).await {
            Ok(session) => {
                let info = session.clone();
                ctx.inner.set_session(Some(session));
                ctx.st.authenticated = true;
                ctx.inner.emit(UiEvent::SessionStarted {
                    user_id: info.user_id.clone(),
                    username: info.username.clone(),
                });
            }
            Err(error) if error.is_auth_error() => {
                let _ = ctx.inner.secrets.delete(secret_keys::SESSION_TOKEN);
                ctx.inner.set_session(None);
                ctx.inner.emit(UiEvent::SessionEnded {
                    reason: error.to_string(),
                });
            }
            Err(error) if is_transport_error(&error) => return Err(SessionError::Retry(error)),
            Err(error) => {
                warn!(error = %error, "resume failed; continuing unauthenticated");
            }
        }
    }

    if ctx.st.authenticated {
        ctx.st.needs_bootstrap = true;
        ctx.inner.set_state(ConnectionState::Connected);
    } else {
        ctx.inner.set_state(ConnectionState::Unauthenticated);
    }

    let mut ticker = interval(inner.config.outbox_tick);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        let wake = tokio::select! {
            biased;
            cmd = commands.recv() => Wake::Command(cmd),
            frame = ctx.transport.recv() => Wake::Frame(frame),
            _ = ticker.tick() => Wake::Tick,
        };
        match wake {
            Wake::Command(None) => return Ok(()),
            Wake::Command(Some(Command::Shutdown { reply })) => {
                let _ = reply.send(());
                return Ok(());
            }
            Wake::Command(Some(command)) => {
                if let Err(error) = ctx.handle_command(command).await {
                    if is_transport_error(&error) {
                        return Err(SessionError::Retry(error));
                    }
                    debug!(error = %error, "command handling failed");
                }
            }
            Wake::Frame(Ok(Some(bytes))) => {
                if let Err(error) = ctx.handle_frame(bytes).await {
                    if is_transport_error(&error) || is_protocol_error(&error) {
                        return Err(SessionError::Retry(error));
                    }
                    warn!(error = %error, "frame handling failed");
                }
            }
            Wake::Frame(Ok(None)) => {
                return Err(SessionError::Retry(CoreError::Transport(
                    crate::error::TransportError::closed("connection closed by server"),
                )))
            }
            Wake::Frame(Err(error)) => {
                return Err(SessionError::Retry(CoreError::Transport(error)))
            }
            Wake::Tick => {
                let result = ctx.tick().await;
                if let Err(error) = result {
                    if is_transport_error(&error) {
                        return Err(SessionError::Retry(error));
                    }
                    warn!(error = %error, "background task failed");
                }
            }
        }
    }
}

/// Структура для последовательной работы с соединением.
struct Ctx<'a> {
    inner: &'a Arc<Inner>,
    transport: &'a mut Box<dyn Transport>,
    st: &'a mut LoopState,
}

impl Ctx<'_> {
    // --- низкоуровневый обмен ---

    async fn request(&mut self, method: request::Method) -> Result<response::Result, CoreError> {
        let id = self.inner.next_request_id();
        let envelope = proto::Envelope::with_request(id, method);
        let bytes = rchat_proto::encode_envelope(&envelope)
            .map_err(|e| CoreError::Internal(format!("encode: {e}")))?;
        let limit = self.frame_limit();
        rchat_proto::check_frame_size(bytes.len(), limit)
            .map_err(|_| CoreError::InvalidInput(format!("frame too large: {}", bytes.len())))?;
        self.transport
            .send(&bytes)
            .await
            .map_err(CoreError::Transport)?;
        self.wait_response(id).await
    }

    fn frame_limit(&self) -> usize {
        let server_limit = self
            .inner
            .server_hello
            .read()
            .expect("hello lock poisoned")
            .as_ref()
            .map(|h| h.max_frame_bytes)
            .filter(|v| *v > 0)
            .unwrap_or(rchat_proto::limits::MAX_FRAME_BYTES_DEFAULT);
        (server_limit as usize).min(self.inner.config.max_frame_bytes as usize)
    }

    async fn wait_response(&mut self, id: u64) -> Result<response::Result, CoreError> {
        let deadline = tokio::time::Instant::now() + self.inner.config.request_timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(CoreError::Timeout);
            }
            let frame = match timeout(remaining, self.transport.recv()).await {
                Err(_) => return Err(CoreError::Timeout),
                Ok(Err(error)) => return Err(CoreError::Transport(error)),
                Ok(Ok(None)) => {
                    return Err(CoreError::Transport(crate::error::TransportError::closed(
                        "connection closed while waiting for response",
                    )))
                }
                Ok(Ok(Some(bytes))) => bytes,
            };
            let envelope = match rchat_proto::decode_envelope(&frame, self.frame_limit()) {
                Ok(envelope) => envelope,
                Err(error) => {
                    self.st.malformed_frames += 1;
                    warn!(%error, "malformed frame");
                    if self.st.malformed_frames > 10 {
                        return Err(CoreError::Protocol("too many malformed frames".into()));
                    }
                    continue;
                }
            };
            match envelope.body {
                Some(envelope::Body::Response(res)) if envelope.request_id == id => {
                    return Ok(res.result.unwrap_or(response::Result::Error(
                        rchat_proto::api_error(proto::ErrorCode::Internal, "empty response", false),
                    )))
                }
                Some(envelope::Body::Response(other)) => {
                    warn!(
                        request_id = envelope.request_id,
                        "unexpected response id while waiting for {id} ({other:?})"
                    );
                }
                Some(envelope::Body::Push(p)) => self.handle_push(p).await?,
                Some(envelope::Body::Error(error)) => return Err(rejected_from(&error)),
                _ => {
                    self.st.malformed_frames += 1;
                    warn!("unexpected envelope while waiting for response");
                }
            }
        }
    }

    /// Обработка кадров в основном цикле (ответы здесь — опоздавшие: запрос
    /// уже завершился тайм-аутом; они считаются безопасно пропущенными).
    async fn handle_frame(&mut self, bytes: Vec<u8>) -> Result<(), CoreError> {
        let limit = self.frame_limit();
        let envelope = match rchat_proto::decode_envelope(&bytes, limit) {
            Ok(envelope) => envelope,
            Err(error) => {
                self.st.malformed_frames += 1;
                warn!(%error, "malformed frame");
                if self.st.malformed_frames > 10 {
                    return Err(CoreError::Protocol("too many malformed frames".into()));
                }
                return Ok(());
            }
        };
        match envelope.body {
            Some(envelope::Body::Push(p)) => self.handle_push(p).await,
            Some(envelope::Body::Error(error)) => {
                if is_fatal_protocol_error(&error) {
                    return Err(rejected_from(&error));
                }
                warn!(code = error.code, message = %error.message, "connection-level error");
                self.inner.emit(UiEvent::Warning(error.message));
                Ok(())
            }
            Some(envelope::Body::Response(response)) => {
                debug!(
                    request_id = envelope.request_id,
                    "late response dropped: {response:?}"
                );
                Ok(())
            }
            other => {
                self.st.malformed_frames += 1;
                warn!("unexpected envelope in main loop: {other:?}");
                Ok(())
            }
        }
    }

    // --- push-события ---

    async fn handle_push(&mut self, push: proto::Push) -> Result<(), CoreError> {
        match push.event {
            Some(push::Event::SyncEvent(event)) => self.handle_sync_event(event).await,
            Some(push::Event::Transient(transient)) => {
                if let Some(transient_event::Event::Presence(presence)) = transient.event {
                    self.inner.emit(UiEvent::PresenceChanged {
                        user_id: presence.user_id,
                        online: presence.online,
                    });
                }
                Ok(())
            }
            None => Ok(()),
        }
    }

    async fn handle_sync_event(&mut self, event: proto::SyncEvent) -> Result<(), CoreError> {
        let cursor = self.inner.storage.get_sync_cursor().await?;
        if event.seq <= cursor {
            return Ok(()); // повторная доставка: безопасно игнорируем
        }
        if event.seq != cursor + 1 {
            // Разрыв в последовательности: догрузим страницы по курсору.
            self.st.needs_sync = true;
            return Ok(());
        }
        let apply = self.event_to_apply(&event).await?;
        self.inner
            .storage
            .apply_sync_batch(std::slice::from_ref(&apply), event.seq)
            .await?;
        self.emit_applied(&apply).await?;
        Ok(())
    }

    /// Преобразует событие журнала в мутацию локального состояния
    /// (с расшифрованием E2E-сообщений).
    async fn event_to_apply(&mut self, event: &proto::SyncEvent) -> Result<SyncApply, CoreError> {
        use sync_event::Payload;
        match event.payload.as_ref() {
            Some(Payload::Message(inner)) => {
                let message = inner
                    .message
                    .as_ref()
                    .ok_or_else(|| CoreError::Protocol("message event without message".into()))?;
                let body = self.decrypt_message(message).await;
                Ok(SyncApply::InsertMessage(convert::message_from_proto(
                    message, body,
                )))
            }
            Some(Payload::MessageEdited(inner)) => {
                let message = inner.message.as_ref().ok_or_else(|| {
                    CoreError::Protocol("message edited event without message".into())
                })?;
                let body = self.decrypt_message(message).await;
                Ok(SyncApply::EditMessage {
                    server_id: message.server_id,
                    content_kind: message.content_kind.clone(),
                    body,
                    edited_at_ms: message.edited_at_ms,
                })
            }
            Some(Payload::MessageDeleted(inner)) => {
                let message = inner.message.as_ref().ok_or_else(|| {
                    CoreError::Protocol("message deleted event without message".into())
                })?;
                Ok(SyncApply::DeleteMessage {
                    server_id: message.server_id,
                    deleted_at_ms: if message.deleted_at_ms == 0 {
                        rchat_proto::now_ms()
                    } else {
                        message.deleted_at_ms
                    },
                })
            }
            Some(Payload::ChatCreated(inner)) => self.chat_snapshot_apply(inner.chat.as_ref()),
            Some(Payload::ChatUpdated(inner)) => self.chat_snapshot_apply(inner.chat.as_ref()),
            Some(Payload::Receipt(inner)) => {
                let delivered = inner.up_to_server_id;
                let read = if inner.read { inner.up_to_server_id } else { 0 };
                Ok(SyncApply::UpsertReceipt(ReceiptRecord {
                    chat_id: inner.chat_id.clone(),
                    user_id: inner.user_id.clone(),
                    delivered_up_to: delivered,
                    read_up_to: read,
                    updated_at_ms: rchat_proto::now_ms(),
                }))
            }
            Some(Payload::DeviceChanged(inner)) => {
                match proto::DeviceChangeKind::try_from(inner.change) {
                    Ok(proto::DeviceChangeKind::Added) => {
                        self.st.device_refresh.insert(inner.user_id.clone());
                        Ok(SyncApply::Skip {
                            seq: event.seq,
                            kind_hint: event.kind_hint,
                        })
                    }
                    Ok(proto::DeviceChangeKind::Revoked) => {
                        if let Some(crypto) = self.inner.crypto.as_ref() {
                            if let Err(error) = crypto.remove_session(&inner.device_id).await {
                                warn!(error = %error, "failed to drop session for revoked device");
                            }
                        }
                        Ok(SyncApply::DeviceRevoked {
                            device_id: inner.device_id.clone(),
                            revoked_at_ms: inner.changed_at_ms,
                        })
                    }
                    _ => Ok(SyncApply::Skip {
                        seq: event.seq,
                        kind_hint: event.kind_hint,
                    }),
                }
            }
            None => {
                // Неизвестный тип события: пропускаем, курсор продвигается.
                info!(
                    seq = event.seq,
                    kind_hint = event.kind_hint,
                    "skipping unknown sync event"
                );
                Ok(SyncApply::Skip {
                    seq: event.seq,
                    kind_hint: event.kind_hint,
                })
            }
        }
    }

    /// Применяет снимок чата из события журнала (chat_created / chat_updated):
    /// обновляет запись чата и состав участников, помечает чужие устройства
    /// к обновлению.
    fn chat_snapshot_apply(&mut self, chat: Option<&proto::Chat>) -> Result<SyncApply, CoreError> {
        let chat = chat.ok_or_else(|| CoreError::Protocol("chat event without chat".into()))?;
        let session = self
            .inner
            .session_info()
            .ok_or(CoreError::NotAuthenticated)?;
        let (record, members) = convert::chat_from_proto(chat, &session.user_id)?;
        for member in &members {
            if member.user_id != session.user_id {
                self.st.device_refresh.insert(member.user_id.clone());
            }
        }
        Ok(SyncApply::UpsertChat {
            chat: record,
            members,
        })
    }

    /// Расшифровывает содержимое сообщения для этого устройства.
    /// Возвращает `None`, если расшифровка невозможна: сообщение остаётся
    /// видимым, но без текста (контролируемая деградация, без обхода проверок).
    async fn decrypt_message(&mut self, message: &proto::Message) -> Option<String> {
        if message.e2e_version == rchat_proto::E2E_VERSION_NONE {
            return if message.plaintext_dev.is_empty() {
                None
            } else {
                Some(message.plaintext_dev.clone())
            };
        }
        let crypto = match self.inner.crypto.as_ref() {
            Some(crypto) => crypto.clone(),
            None => {
                warn!(
                    server_id = message.server_id,
                    "e2e message but no crypto provider"
                );
                return None;
            }
        };
        if message.ciphertext.is_empty() {
            return None; // нет копии для этого устройства (например, история с другим устройством)
        }
        let cached = self
            .inner
            .storage
            .get_device(&message.sender_device_id)
            .await
            .ok()
            .flatten();
        if let Some(device) = cached.as_ref() {
            if !device.curve25519.is_empty()
                && !message.sender_curve25519.is_empty()
                && device.curve25519 != message.sender_curve25519
            {
                warn!(
                    device_id = %message.sender_device_id,
                    "identity key mismatch for sender device; refusing to decrypt"
                );
                self.inner.emit(UiEvent::E2eDecryptFailed {
                    chat_id: message.chat_id.clone(),
                    server_id: message.server_id,
                    device_id: message.sender_device_id.clone(),
                });
                return None;
            }
        }
        let sender = PeerDevice {
            user_id: message.sender_user_id.clone(),
            device_id: message.sender_device_id.clone(),
            ed25519: cached
                .as_ref()
                .map(|d| d.ed25519.clone())
                .unwrap_or_default(),
            curve25519: message.sender_curve25519.clone(),
            one_time_key: None,
        };
        let encrypted = crate::traits::EncryptedMessage {
            device_id: self
                .inner
                .session_info()
                .map(|s| s.device_id)
                .unwrap_or_default(),
            ciphertext: message.ciphertext.clone(),
            e2e_version: message.e2e_version,
            olm_type: proto::OlmMessageType::try_from(message.olm_type)
                .unwrap_or(proto::OlmMessageType::Normal),
            sender_curve25519: message.sender_curve25519.clone(),
        };
        match crypto.decrypt_from(&sender, &encrypted).await {
            Ok(plaintext) => match String::from_utf8(plaintext) {
                Ok(text) => Some(text),
                Err(_) => {
                    warn!(
                        server_id = message.server_id,
                        "decrypted message is not utf-8"
                    );
                    None
                }
            },
            Err(error) => {
                warn!(error = %error, server_id = message.server_id, "decrypt failed");
                self.inner.emit(UiEvent::E2eDecryptFailed {
                    chat_id: message.chat_id.clone(),
                    server_id: message.server_id,
                    device_id: message.sender_device_id.clone(),
                });
                None
            }
        }
    }

    /// Публикует события UI после успешного применения мутации.
    async fn emit_applied(&mut self, apply: &SyncApply) -> Result<(), CoreError> {
        match apply {
            SyncApply::InsertMessage(message) => {
                self.inner.emit(UiEvent::MessageReceived(message.clone()));
                let session = self.inner.session_info();
                let is_own = session
                    .as_ref()
                    .map(|s| s.user_id == message.sender_user_id)
                    .unwrap_or(false);
                if !is_own && self.inner.config.auto_mark_delivered {
                    if let Some(server_id) = message.server_id {
                        let entry = self
                            .st
                            .pending_receipts
                            .entry(message.chat_id.clone())
                            .or_insert(0);
                        *entry = (*entry).max(server_id);
                    }
                }
            }
            SyncApply::EditMessage { server_id, .. }
            | SyncApply::DeleteMessage { server_id, .. } => {
                if let Some(record) = self.inner.storage.get_message_by_server(*server_id).await? {
                    self.inner.emit(UiEvent::MessageUpdated(record));
                }
            }
            SyncApply::UpsertChat { chat, .. } => {
                self.inner.emit(UiEvent::ChatUpserted(chat.clone()));
            }
            SyncApply::UpsertReceipt(receipt) => {
                self.inner.emit(UiEvent::ReceiptUpdated(receipt.clone()));
            }
            SyncApply::DeviceRevoked { device_id, .. } => {
                self.inner
                    .emit(UiEvent::Warning(format!("device {device_id} was revoked")));
            }
            SyncApply::UpsertUser(_) | SyncApply::UpsertDevice(_) | SyncApply::Skip { .. } => {}
        }
        Ok(())
    }

    // --- периодические задачи ---

    async fn tick(&mut self) -> Result<(), CoreError> {
        if self.st.needs_bootstrap && self.st.authenticated {
            self.bootstrap().await?;
        }
        if self.st.needs_sync && self.st.authenticated {
            match self.run_sync().await {
                Ok(_) => {}
                Err(error) if is_transport_error(&error) => return Err(error),
                Err(error) if error.is_auth_error() => {
                    warn!(error = %error, "sync rejected: session lost");
                    self.mark_session_lost(&error).await;
                }
                Err(error) => warn!(error = %error, "sync failed; will retry"),
            }
        }
        if self.st.authenticated && !self.st.outbox_paused {
            self.flush_outbox().await?;
        }
        if self.st.authenticated {
            self.flush_receipts().await?;
            self.flush_device_refresh().await?;
        }
        Ok(())
    }

    async fn bootstrap(&mut self) -> Result<(), CoreError> {
        self.st.needs_bootstrap = false;
        if !self.st.authenticated {
            return Ok(());
        }
        self.inner.storage.reset_inflight_outbox().await?;
        self.sync_prekeys().await?;
        let chats = self.inner.storage.list_chats().await?;
        if chats.is_empty() {
            // Новое устройство: получаем согласованный snapshot.
            self.perform_snapshot().await?;
        }
        self.st.needs_sync = true;
        match self.run_sync().await {
            Ok(_) => Ok(()),
            Err(error) if error.is_auth_error() => {
                self.mark_session_lost(&error).await;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// Догружает пул prekey и публикует их.
    async fn sync_prekeys(&mut self) -> Result<(), CoreError> {
        let Some(crypto) = self.inner.crypto.as_ref().cloned() else {
            return Ok(());
        };
        let upload = match crypto
            .keys_for_upload(self.inner.config.prekeys_target)
            .await
        {
            Ok(upload) => upload,
            Err(error) => {
                warn!(error = %error, "prekey generation failed");
                return Ok(());
            }
        };
        if upload.one_time_keys.is_empty() && upload.fallback_key.is_none() {
            return Ok(());
        }
        let method = request::Method::UploadKeys(proto::UploadKeysRequest {
            one_time_keys: upload.one_time_keys.iter().map(|k| k.as_proto()).collect(),
            fallback_key: upload.fallback_key.as_ref().map(|k| k.as_proto()),
            e2e_version: crypto.e2e_version(),
        });
        match self.request(method).await {
            Ok(response::Result::UploadKeys(_)) => {
                if let Err(error) = crypto.mark_published().await {
                    warn!(error = %error, "failed to mark prekeys as published");
                }
            }
            Ok(response::Result::Error(error)) => {
                warn!(code = error.code, "server rejected prekey upload");
            }
            Ok(other) => warn!("unexpected response to prekey upload: {other:?}"),
            Err(error) if is_transport_error(&error) => return Err(error),
            Err(error) => warn!(error = %error, "prekey upload failed"),
        }
        Ok(())
    }

    // --- синхронизация ---

    async fn run_sync(&mut self) -> Result<SyncProgress, CoreError> {
        let mut progress = SyncProgress {
            from_seq: self.inner.storage.get_sync_cursor().await?,
            ..Default::default()
        };
        let mut pages = 0u32;
        loop {
            pages += 1;
            if pages > 1000 {
                warn!("sync page limit reached; continuing on next tick");
                break;
            }
            let cursor = self.inner.storage.get_sync_cursor().await?;
            let limit = self.sync_page_limit();
            let method = request::Method::Sync(proto::SyncRequest {
                after_seq: cursor,
                limit,
            });
            let result = match self.request(method).await {
                Ok(response::Result::Sync(result)) => result,
                Ok(response::Result::Error(error)) => {
                    if proto::ErrorCode::try_from(error.code).ok()
                        == Some(proto::ErrorCode::ResyncRequired)
                    {
                        self.perform_snapshot().await?;
                        progress.resync_performed = true;
                        continue;
                    }
                    return Err(rejected_from(&error));
                }
                Ok(other) => {
                    return Err(CoreError::Protocol(format!(
                        "unexpected response to sync: {other:?}"
                    )))
                }
                Err(error) => return Err(error),
            };

            if result.resync_required {
                info!("server requires resync; performing snapshot");
                self.perform_snapshot().await?;
                progress.resync_performed = true;
                continue;
            }

            let mut batch: Vec<SyncApply> = Vec::new();
            let mut expected = cursor;
            for event in &result.events {
                if event.seq <= expected {
                    continue; // дубликат (повторная доставка/гонка)
                }
                if event.seq != expected + 1 {
                    warn!(
                        expected = expected + 1,
                        got = event.seq,
                        "gap in sync page; pulling again"
                    );
                    break;
                }
                let apply = self.event_to_apply(event).await?;
                if matches!(apply, SyncApply::Skip { .. }) {
                    progress.events_skipped += 1;
                } else {
                    progress.events_applied += 1;
                }
                batch.push(apply);
                expected = event.seq;
            }

            let page_len = batch.len() as u32;
            if batch.is_empty() {
                if result.up_to_seq > cursor {
                    self.inner
                        .storage
                        .apply_sync_batch(&[], result.up_to_seq)
                        .await?;
                }
                progress.to_seq = cursor.max(result.up_to_seq);
                break;
            }
            self.inner
                .storage
                .apply_sync_batch(&batch, expected)
                .await?;
            for apply in &batch {
                self.emit_applied(apply).await?;
            }
            progress.to_seq = expected;
            if page_len < limit.max(1) || result.up_to_seq <= expected {
                if result.up_to_seq > expected {
                    continue;
                }
                break;
            }
        }
        self.st.needs_sync = false;
        self.inner.emit(UiEvent::SyncProgress(progress));
        Ok(progress)
    }

    fn sync_page_limit(&self) -> u32 {
        let server_limit = self
            .inner
            .server_hello
            .read()
            .expect("hello lock poisoned")
            .as_ref()
            .map(|h| h.max_sync_page)
            .filter(|v| *v > 0)
            .unwrap_or(rchat_proto::limits::MAX_SYNC_PAGE_DEFAULT);
        let configured = self.inner.config.sync_page_limit;
        if configured == 0 {
            server_limit
        } else {
            configured.min(server_limit)
        }
    }

    /// Snapshot после `ResyncRequired` или для нового устройства.
    async fn perform_snapshot(&mut self) -> Result<(), CoreError> {
        let snapshot = match self
            .request(request::Method::GetSnapshot(proto::SnapshotRequest {}))
            .await?
        {
            response::Result::GetSnapshot(snapshot) => snapshot,
            response::Result::Error(error) => return Err(rejected_from(&error)),
            other => {
                return Err(CoreError::Protocol(format!(
                    "unexpected response to snapshot: {other:?}"
                )))
            }
        };
        let session = self
            .inner
            .session_info()
            .ok_or(CoreError::NotAuthenticated)?;

        for user in &snapshot.users {
            self.inner
                .storage
                .upsert_user(&convert::user_from_proto(user))
                .await?;
        }
        for device in &snapshot.devices {
            let record = convert::device_from_proto(device)?;
            if !record.user_id.is_empty() {
                self.inner.storage.upsert_device(&record).await?;
                if !record.ed25519.is_empty() {
                    // своё устройство уже известно; чужое — обновляем кеш
                    if record.user_id != session.user_id {
                        self.st.device_refresh.insert(record.user_id.clone());
                    }
                }
            }
        }
        let mut chats: Vec<(crate::types::ChatRecord, Vec<crate::types::MemberRecord>)> =
            Vec::with_capacity(snapshot.chats.len());
        for chat in &snapshot.chats {
            let (record, members) = convert::chat_from_proto(chat, &session.user_id)?;
            for member in &members {
                if member.user_id != session.user_id {
                    self.st.device_refresh.insert(member.user_id.clone());
                }
            }
            chats.push((record, members));
        }
        self.inner.storage.replace_chats(&chats).await?;
        // Догружаем последние сообщения по каждому чату (первая страница).
        for (chat, _) in &chats {
            match self
                .fetch_history_inner(&chat.chat_id, None, self.history_page_limit())
                .await
            {
                Ok(_) => {}
                Err(error) if is_transport_error(&error) => return Err(error),
                Err(error) => {
                    warn!(chat_id = %chat.chat_id, error = %error, "history fetch failed")
                }
            }
        }
        // Курсор продвигаем последним: сбой до этого места просто повторит snapshot.
        self.inner
            .storage
            .set_sync_cursor(snapshot.up_to_seq)
            .await?;
        self.inner.emit(UiEvent::ResyncPerformed {
            up_to_seq: snapshot.up_to_seq,
        });
        Ok(())
    }

    fn history_page_limit(&self) -> u32 {
        self.inner
            .server_hello
            .read()
            .expect("hello lock poisoned")
            .as_ref()
            .map(|h| h.max_history_page)
            .filter(|v| *v > 0)
            .unwrap_or(rchat_proto::limits::MAX_HISTORY_PAGE_DEFAULT)
    }

    /// Загружает страницу истории, расшифровывает и сохраняет её.
    /// Курсор синхронизации не двигается.
    async fn fetch_history_inner(
        &mut self,
        chat_id: &str,
        before_server_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<MessageRecord>, CoreError> {
        let messages = match self
            .request(request::Method::GetHistory(proto::GetHistoryRequest {
                chat_id: chat_id.to_string(),
                before_server_id: before_server_id.unwrap_or(0),
                limit,
            }))
            .await?
        {
            response::Result::GetHistory(result) => result.messages,
            response::Result::Error(error) => return Err(rejected_from(&error)),
            other => {
                return Err(CoreError::Protocol(format!(
                    "unexpected response to history: {other:?}"
                )))
            }
        };
        let cursor = self.inner.storage.get_sync_cursor().await?;
        let mut batch = Vec::new();
        let mut records = Vec::with_capacity(messages.len());
        for message in &messages {
            if let Some(existing) = self
                .inner
                .storage
                .get_message_by_server(message.server_id)
                .await?
            {
                records.push(existing);
                continue;
            }
            let body = self.decrypt_message(message).await;
            let record = convert::message_from_proto(message, body);
            batch.push(SyncApply::InsertMessage(record.clone()));
            records.push(record);
        }
        self.inner.storage.apply_sync_batch(&batch, cursor).await?;
        Ok(records)
    }

    // --- outbox ---

    async fn flush_outbox(&mut self) -> Result<(), CoreError> {
        if !self.st.authenticated {
            return Ok(());
        }
        let now = rchat_proto::now_ms();
        let due = self.inner.storage.due_outbox(now, 8).await?;
        for mut item in due {
            if !self.st.authenticated {
                break;
            }
            item.state = OutboxState::Sending;
            item.attempts = item.attempts.saturating_add(1);
            self.inner.storage.update_outbox(&item).await?;

            let send_request = match prost::Message::decode(&item.request[..]) {
                Ok(request) => request,
                Err(error) => {
                    warn!(error = %error, local_id = %item.local_id, "corrupt outbox entry");
                    self.inner
                        .storage
                        .apply_outbox_failure(
                            &item.local_id,
                            OutboxState::Failed,
                            None,
                            "corrupt outbox record",
                            0,
                        )
                        .await?;
                    self.emit_outbox(
                        &item.local_id,
                        OutboxState::Failed,
                        Some("corrupt outbox record".into()),
                    );
                    continue;
                }
            };
            let method = request::Method::SendMessage(send_request);
            let outcome = self.request(method).await;
            match outcome {
                Ok(response::Result::SendMessage(result)) => {
                    let server_id =
                        result
                            .message
                            .as_ref()
                            .map(|m| m.server_id)
                            .ok_or_else(|| {
                                CoreError::Protocol("send response without message".into())
                            })?;
                    self.inner
                        .storage
                        .apply_send_ack(&item.local_id, server_id)
                        .await?;
                    self.emit_outbox(&item.local_id, OutboxState::Sent, None);
                    if let Some(record) = self.inner.storage.get_message(&item.local_id).await? {
                        self.inner.emit(UiEvent::MessageUpdated(record));
                    }
                }
                Ok(response::Result::Error(error)) => {
                    let code = proto::ErrorCode::try_from(error.code).ok();
                    if code == Some(proto::ErrorCode::E2eKeysMissing) && item.attempts <= 8 {
                        // Набор устройств получателя изменился: пересобираем
                        // шифротекст с актуальными ключами.
                        match self.rebuild_outbox_item(&mut item).await {
                            Ok(()) => {
                                item.state = OutboxState::Pending;
                                item.next_attempt_at_ms = rchat_proto::now_ms() + 200;
                                item.last_error_code = Some(error.code);
                                item.last_error = Some(error.message.clone());
                                self.inner.storage.update_outbox(&item).await?;
                                self.emit_outbox(
                                    &item.local_id,
                                    OutboxState::Pending,
                                    Some(error.message),
                                );
                            }
                            Err(rebuild_error) => {
                                warn!(error = %rebuild_error, "failed to rebuild outbox entry");
                                self.fail_outbox_item(&item, error.code, &error.message)
                                    .await?;
                            }
                        }
                        continue;
                    }
                    if error.retryable && !is_auth_code(code) {
                        let delay = delay_for_attempt(&self.inner.config.backoff, item.attempts);
                        self.inner
                            .storage
                            .apply_outbox_failure(
                                &item.local_id,
                                OutboxState::Pending,
                                Some(error.code),
                                &error.message,
                                rchat_proto::now_ms() + delay.as_millis() as u64,
                            )
                            .await?;
                        self.emit_outbox(&item.local_id, OutboxState::Pending, Some(error.message));
                    } else {
                        self.fail_outbox_item(&item, error.code, &error.message)
                            .await?;
                        let rejected = rejected_from(&error);
                        if rejected.is_auth_error() {
                            self.mark_session_lost(&rejected).await;
                            break;
                        }
                    }
                }
                Ok(other) => {
                    let message = format!("unexpected send response: {other:?}");
                    warn!(%message);
                    self.fail_outbox_item(&item, 0, &message).await?;
                }
                Err(error) if is_transport_error(&error) => {
                    // Соединение потеряно: вернуть в очередь, сессия перезапустится.
                    let delay = delay_for_attempt(&self.inner.config.backoff, item.attempts);
                    self.inner
                        .storage
                        .apply_outbox_failure(
                            &item.local_id,
                            OutboxState::Pending,
                            None,
                            "connection lost",
                            rchat_proto::now_ms() + delay.as_millis() as u64,
                        )
                        .await?;
                    self.emit_outbox(
                        &item.local_id,
                        OutboxState::Pending,
                        Some("connection lost".into()),
                    );
                    return Err(error);
                }
                Err(error) => {
                    // Тайм-аут и прочие: сообщение могло быть принято сервером —
                    // повтор с тем же ключом идемпотентности безопасен.
                    let delay = delay_for_attempt(&self.inner.config.backoff, item.attempts);
                    let text = error.to_string();
                    self.inner
                        .storage
                        .apply_outbox_failure(
                            &item.local_id,
                            OutboxState::Pending,
                            None,
                            &text,
                            rchat_proto::now_ms() + delay.as_millis() as u64,
                        )
                        .await?;
                    self.emit_outbox(&item.local_id, OutboxState::Pending, Some(text));
                }
            }
        }
        Ok(())
    }

    async fn fail_outbox_item(
        &mut self,
        item: &crate::types::OutboxRecord,
        code: i32,
        message: &str,
    ) -> Result<(), CoreError> {
        self.inner
            .storage
            .apply_outbox_failure(&item.local_id, OutboxState::Failed, Some(code), message, 0)
            .await?;
        self.emit_outbox(
            &item.local_id,
            OutboxState::Failed,
            Some(message.to_string()),
        );
        Ok(())
    }

    fn emit_outbox(&self, local_id: &str, state: OutboxState, error: Option<String>) {
        self.inner.emit(UiEvent::OutboxChanged {
            local_id: local_id.to_string(),
            state,
            error,
        });
    }

    /// Пересобирает запрос для записи outbox (например, после появления новых
    /// устройств у получателя): перечитывает открытый текст, получает свежие
    /// prekey и шифрует заново.
    async fn rebuild_outbox_item(
        &mut self,
        item: &mut crate::types::OutboxRecord,
    ) -> Result<(), CoreError> {
        let message = self
            .inner
            .storage
            .get_message(&item.local_id)
            .await?
            .ok_or_else(|| CoreError::Internal("outbox message missing".into()))?;
        let text = message
            .body
            .clone()
            .ok_or_else(|| CoreError::Internal("message body missing; cannot rebuild".into()))?;
        let chat = self
            .inner
            .storage
            .get_chat(&item.chat_id)
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("chat {}", item.chat_id)))?;
        let account: LocalAccount = self
            .inner
            .storage
            .load_account()
            .await?
            .ok_or(CoreError::NotAuthenticated)?;

        // Пересборка E2E-запроса требует актуального кэша устройств
        // участников: набор мог измениться (новое устройство, отзыв).
        // Обновление best-effort: если оно не удалось, compose решит сам,
        // а сервер при неполном покрытии вернёт E2eKeysMissing.
        if chat.e2e_mode == proto::E2eMode::OlmV1 {
            let members = self.inner.storage.chat_members(&chat.chat_id).await?;
            let user_ids: Vec<String> = members.iter().map(|m| m.user_id.clone()).collect();
            if let Err(error) = self.refresh_devices_batch(&user_ids).await {
                warn!(error = %error, "device refresh before outbox rebuild failed");
            }
        }

        let inner = Arc::clone(self.inner);
        let this: &mut Self = self;
        let fetch = move |ids: Vec<String>| async move { this.fetch_bundles(&ids).await };
        let request = crate::send::compose_send_request(
            &inner,
            &item.local_id,
            fetch,
            &chat,
            &account,
            &text,
            message.reply_to_server_id,
        )
        .await?;
        let mut bytes = Vec::new();
        prost::Message::encode(&request, &mut bytes)
            .map_err(|e| CoreError::Internal(format!("encode: {e}")))?;
        item.request = bytes;
        item.state = OutboxState::Pending;
        self.inner.storage.update_outbox(item).await?;
        Ok(())
    }

    async fn fetch_bundles(&mut self, device_ids: &[String]) -> Result<Vec<PeerDevice>, CoreError> {
        let result = match self
            .request(request::Method::GetDeviceBundles(
                proto::GetDeviceBundlesRequest {
                    device_ids: device_ids.to_vec(),
                },
            ))
            .await?
        {
            response::Result::GetDeviceBundles(result) => result,
            response::Result::Error(error) => return Err(rejected_from(&error)),
            other => {
                return Err(CoreError::Protocol(format!(
                    "unexpected response to bundles: {other:?}"
                )))
            }
        };
        Ok(convert::peer_devices_from_bundles(&result.bundles))
    }

    // --- receipts и обновление устройств ---

    async fn flush_receipts(&mut self) -> Result<(), CoreError> {
        let pending: Vec<(String, u64)> = self
            .st
            .pending_receipts
            .iter()
            .map(|(chat, up_to)| (chat.clone(), *up_to))
            .collect();
        for (chat_id, up_to) in pending {
            let method = request::Method::MarkReceipt(proto::MarkReceiptRequest {
                chat_id: chat_id.clone(),
                up_to_server_id: up_to,
                read: false,
            });
            match self.request(method).await {
                Ok(response::Result::MarkReceipt(_)) => {
                    self.st.pending_receipts.remove(&chat_id);
                }
                Ok(other) => {
                    warn!("unexpected receipt response: {other:?}");
                    self.st.pending_receipts.remove(&chat_id);
                }
                Err(error) if is_transport_error(&error) => return Err(error),
                Err(error) => {
                    warn!(error = %error, "delivered receipt failed");
                    self.st.pending_receipts.remove(&chat_id);
                }
            }
        }
        Ok(())
    }

    /// Запрашивает устройства указанных пользователей одним запросом и
    /// обновляет локальный кэш. Сервер молча пропускает чужих пользователей
    /// без общего чата, поэтому батч безопасен.
    async fn refresh_devices_batch(&mut self, user_ids: &[String]) -> Result<(), CoreError> {
        if user_ids.is_empty() {
            return Ok(());
        }
        // Сервер ограничивает число пользователей в запросе — идём чанками.
        let chunk_size = rchat_proto::limits::MAX_USER_IDS_PER_REQUEST.max(1);
        for chunk in user_ids.chunks(chunk_size) {
            let result = match self
                .request(request::Method::GetDevices(proto::GetDevicesRequest {
                    user_ids: chunk.to_vec(),
                    include_own: true,
                }))
                .await?
            {
                response::Result::GetDevices(result) => result,
                response::Result::Error(error) => return Err(rejected_from(&error)),
                other => {
                    return Err(CoreError::Protocol(format!(
                        "unexpected devices response: {other:?}"
                    )))
                }
            };
            for device in &result.devices {
                let record = convert::device_from_proto(device)?;
                if !record.user_id.is_empty() {
                    self.inner.storage.upsert_device(&record).await?;
                }
            }
        }
        Ok(())
    }

    async fn flush_device_refresh(&mut self) -> Result<(), CoreError> {
        let user_ids: Vec<String> = self.st.device_refresh.iter().cloned().collect();
        if user_ids.is_empty() {
            return Ok(());
        }
        match self.refresh_devices_batch(&user_ids).await {
            Ok(()) => {
                self.st.device_refresh.clear();
                Ok(())
            }
            Err(error) if is_transport_error(&error) => Err(error),
            Err(error) => {
                warn!(error = %error, "device refresh failed");
                self.st.device_refresh.clear();
                Ok(())
            }
        }
    }

    // --- команды ---

    async fn handle_command(&mut self, command: Command) -> Result<(), CoreError> {
        match command {
            Command::Request { method, reply } => {
                let result = if self.st.authenticated {
                    self.request(*method).await
                } else {
                    Err(CoreError::NotAuthenticated)
                };
                if let Err(error) = &result {
                    if error.is_auth_error() {
                        self.mark_session_lost(error).await;
                    }
                }
                let _ = reply.send(result);
                Ok(())
            }
            Command::Register {
                username,
                display_name,
                password,
                reply,
            } => {
                let result = self.register(&username, &display_name, &password).await;
                let _ = reply.send(result);
                Ok(())
            }
            Command::Login {
                username,
                password,
                reply,
            } => {
                let result = self.login(&username, &password).await;
                let _ = reply.send(result);
                Ok(())
            }
            Command::SyncNow { reply } => {
                let result = if self.st.authenticated {
                    self.run_sync().await
                } else {
                    Err(CoreError::NotAuthenticated)
                };
                let _ = reply.send(result);
                Ok(())
            }
            Command::FetchHistory {
                chat_id,
                before_server_id,
                limit,
                reply,
            } => {
                let result = if self.st.authenticated {
                    let limit = if limit == 0 {
                        self.history_page_limit()
                    } else {
                        limit.min(self.history_page_limit())
                    };
                    self.fetch_history_inner(&chat_id, before_server_id, limit)
                        .await
                } else {
                    Err(CoreError::NotAuthenticated)
                };
                let _ = reply.send(result);
                Ok(())
            }
            Command::Logout { wipe, reply } => {
                if self.st.authenticated {
                    // best-effort: серверный отзыв не должен блокировать локальный выход
                    let _ = self
                        .request(request::Method::Logout(proto::LogoutRequest {}))
                        .await;
                }
                let result = logout_local(self.inner, wipe).await;
                self.st.authenticated = false;
                self.st.outbox_paused = true;
                let _ = reply.send(result);
                Ok(())
            }
            Command::Kick => {
                if self.st.authenticated && !self.st.outbox_paused {
                    self.flush_outbox().await?;
                }
                Ok(())
            }
            Command::Shutdown { reply } => {
                let _ = reply.send(());
                Ok(())
            }
        }
    }

    async fn mark_session_lost(&mut self, error: &CoreError) {
        let _ = self.inner.secrets.delete(secret_keys::SESSION_TOKEN);
        self.inner.set_session(None);
        self.st.authenticated = false;
        self.st.outbox_paused = true;
        self.inner.set_state(ConnectionState::Unauthenticated);
        self.inner.emit(UiEvent::SessionEnded {
            reason: error.to_string(),
        });
    }

    // --- регистрация и вход ---

    async fn build_device_info(&mut self) -> Result<proto::DeviceInfo, CoreError> {
        let device_id = self.inner.device_id()?;
        let base = proto::DeviceInfo {
            device_id,
            name: self.inner.config.device_name.clone(),
            platform: self.inner.config.device_platform.clone(),
            keys: None,
            e2e_version: rchat_proto::E2E_VERSION_NONE,
            created_at_ms: 0,
            last_seen_at_ms: 0,
            revoked_at_ms: 0,
            user_id: String::new(),
        };
        match self.inner.crypto.as_ref() {
            Some(crypto) => {
                let (ed25519, curve25519) = crypto.identity_keys().await?;
                Ok(proto::DeviceInfo {
                    keys: Some(proto::DeviceKeys {
                        ed25519,
                        curve25519,
                    }),
                    e2e_version: crypto.e2e_version(),
                    ..base
                })
            }
            None => Ok(base),
        }
    }

    async fn prekeys_for_request(
        &mut self,
    ) -> Result<(Vec<proto::PreKey>, Option<proto::PreKey>), CoreError> {
        match self.inner.crypto.as_ref() {
            Some(crypto) => {
                let upload = crypto
                    .keys_for_upload(self.inner.config.prekeys_target)
                    .await?;
                Ok((
                    upload.one_time_keys.iter().map(|k| k.as_proto()).collect(),
                    upload.fallback_key.as_ref().map(|k| k.as_proto()),
                ))
            }
            None => Ok((Vec::new(), None)),
        }
    }

    async fn register(
        &mut self,
        username: &str,
        display_name: &str,
        password: &str,
    ) -> Result<(), CoreError> {
        if self.st.authenticated {
            return Err(CoreError::InvalidInput("already authenticated".into()));
        }
        let device = self.build_device_info().await?;
        let (one_time_keys, fallback_key) = self.prekeys_for_request().await?;
        let device_id = device.device_id.clone();
        let method = request::Method::Register(proto::RegisterRequest {
            username: username.to_string(),
            display_name: display_name.to_string(),
            password: password.to_string(),
            device: Some(device),
            one_time_keys,
            fallback_key,
        });
        let result = match self.request(method).await? {
            response::Result::Register(result) => result,
            response::Result::Error(error) => return Err(rejected_from(&error)),
            other => {
                return Err(CoreError::Protocol(format!(
                    "unexpected register response: {other:?}"
                )))
            }
        };
        self.establish_session(
            &result.user_id,
            username,
            display_name,
            &device_id,
            &result.session_token,
            result.session_expires_at_ms,
            result.device.as_ref(),
        )
        .await
    }

    async fn login(&mut self, username: &str, password: &str) -> Result<(), CoreError> {
        if self.st.authenticated {
            return Err(CoreError::InvalidInput("already authenticated".into()));
        }
        let device = self.build_device_info().await?;
        let (one_time_keys, fallback_key) = self.prekeys_for_request().await?;
        let device_id = device.device_id.clone();
        let method = request::Method::Login(proto::LoginRequest {
            username: username.to_string(),
            password: password.to_string(),
            device: Some(device),
            one_time_keys,
            fallback_key,
        });
        let result = match self.request(method).await? {
            response::Result::Login(result) => result,
            response::Result::Error(error) => return Err(rejected_from(&error)),
            other => {
                return Err(CoreError::Protocol(format!(
                    "unexpected login response: {other:?}"
                )))
            }
        };
        for device in &result.devices {
            let record = convert::device_from_proto(device)?;
            if !record.user_id.is_empty() {
                self.inner.storage.upsert_device(&record).await?;
            }
        }
        let display_name = self
            .inner
            .storage
            .load_account()
            .await?
            .map(|a| a.display_name)
            .unwrap_or_else(|| username.to_string());
        self.establish_session(
            &result.user_id,
            username,
            &display_name,
            &device_id,
            &result.session_token,
            result.session_expires_at_ms,
            result.device.as_ref(),
        )
        .await
    }

    async fn resume(&mut self, token: &str) -> Result<SessionInfo, CoreError> {
        let method = request::Method::Resume(proto::ResumeRequest {
            session_token: token.to_string(),
        });
        let result = match self.request(method).await? {
            response::Result::Resume(result) => result,
            response::Result::Error(error) => return Err(rejected_from(&error)),
            other => {
                return Err(CoreError::Protocol(format!(
                    "unexpected resume response: {other:?}"
                )))
            }
        };
        let user = result
            .user
            .as_ref()
            .ok_or_else(|| CoreError::Protocol("resume without user".into()))?;
        let device = result
            .device
            .as_ref()
            .ok_or_else(|| CoreError::Protocol("resume without device".into()))?;
        let device_record = convert::device_from_proto(device)?;
        if !device_record.user_id.is_empty() {
            self.inner.storage.upsert_device(&device_record).await?;
        }
        let account = LocalAccount {
            user_id: user.user_id.clone(),
            username: user.username.clone(),
            display_name: user.display_name.clone(),
            device_id: device.device_id.clone(),
        };
        self.inner.storage.save_account(&account).await?;
        self.inner
            .storage
            .upsert_user(&convert::user_from_proto(user))
            .await?;
        Ok(SessionInfo {
            user_id: user.user_id.clone(),
            username: user.username.clone(),
            display_name: user.display_name.clone(),
            device_id: device.device_id.clone(),
            session_expires_at_ms: result.session_expires_at_ms,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn establish_session(
        &mut self,
        user_id: &str,
        username: &str,
        display_name: &str,
        device_id: &str,
        session_token: &str,
        session_expires_at_ms: u64,
        server_device: Option<&proto::DeviceInfo>,
    ) -> Result<(), CoreError> {
        self.inner
            .secrets
            .set(secret_keys::SESSION_TOKEN, session_token.as_bytes())?;
        let account = LocalAccount {
            user_id: user_id.to_string(),
            username: username.to_string(),
            display_name: display_name.to_string(),
            device_id: device_id.to_string(),
        };
        self.inner.storage.save_account(&account).await?;
        self.inner
            .storage
            .upsert_user(&crate::types::UserRecord {
                user_id: user_id.to_string(),
                username: username.to_string(),
                display_name: display_name.to_string(),
            })
            .await?;
        if let Some(device) = server_device {
            let record = convert::device_from_proto(device)?;
            let record = if record.user_id.is_empty() {
                crate::types::DeviceRecord {
                    user_id: user_id.to_string(),
                    ..record
                }
            } else {
                record
            };
            self.inner.storage.upsert_device(&record).await?;
        }
        let info = SessionInfo {
            user_id: user_id.to_string(),
            username: username.to_string(),
            display_name: display_name.to_string(),
            device_id: device_id.to_string(),
            session_expires_at_ms,
        };
        self.inner.set_session(Some(info.clone()));
        self.st.authenticated = true;
        self.st.outbox_paused = false;
        self.st.needs_bootstrap = true;
        self.inner.set_state(ConnectionState::Connected);
        if let Some(crypto) = self.inner.crypto.as_ref() {
            if let Err(error) = crypto.mark_published().await {
                warn!(error = %error, "mark_published failed");
            }
        }
        self.inner.emit(UiEvent::SessionStarted {
            user_id: info.user_id,
            username: info.username,
        });
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Handshake
// ---------------------------------------------------------------------------

fn client_capabilities(inner: &Inner) -> Vec<String> {
    let mut caps = vec![
        rchat_proto::caps::RECEIPTS.to_string(),
        rchat_proto::caps::PRESENCE.to_string(),
        rchat_proto::caps::EDIT.to_string(),
        rchat_proto::caps::DELETE.to_string(),
        rchat_proto::caps::MULTI_DEVICE.to_string(),
        rchat_proto::caps::HISTORY.to_string(),
        rchat_proto::caps::SNAPSHOT.to_string(),
    ];
    if inner.crypto.is_some() {
        caps.push(rchat_proto::caps::E2E_OLM_V1.to_string());
    }
    caps
}

fn required_capabilities(inner: &Inner) -> Vec<String> {
    let mut required = inner.config.required_capabilities.clone();
    if inner.config.e2e_enabled {
        required.push(rchat_proto::caps::E2E_OLM_V1.to_string());
    }
    required
}

fn is_fatal_protocol_error(error: &proto::Error) -> bool {
    matches!(
        proto::ErrorCode::try_from(error.code).ok(),
        Some(
            proto::ErrorCode::ProtocolVersionMismatch
                | proto::ErrorCode::UnsupportedCapability
                | proto::ErrorCode::PayloadTooLarge
        )
    )
}

fn is_auth_code(code: Option<proto::ErrorCode>) -> bool {
    matches!(
        code,
        Some(
            proto::ErrorCode::Unauthenticated
                | proto::ErrorCode::SessionExpired
                | proto::ErrorCode::SessionRevoked
                | proto::ErrorCode::InvalidCredentials
        )
    )
}

async fn handshake(
    inner: &Arc<Inner>,
    transport: &mut Box<dyn Transport>,
) -> Result<proto::ServerHello, SessionError> {
    let hello = proto::ClientHello {
        min_major: rchat_proto::PROTOCOL_MAJOR,
        max_major: rchat_proto::PROTOCOL_MAJOR,
        min_minor: 0,
        max_minor: rchat_proto::PROTOCOL_MINOR,
        capabilities: client_capabilities(inner),
        required_capabilities: required_capabilities(inner),
        max_frame_bytes: inner.config.max_frame_bytes,
        client_name: inner.config.client_name.clone(),
        client_version: inner.config.client_version.clone(),
    };
    let envelope = proto::Envelope {
        request_id: 0,
        body: Some(envelope::Body::ClientHello(hello)),
    };
    let bytes = rchat_proto::encode_envelope(&envelope)
        .map_err(|e| SessionError::Fatal(CoreError::Internal(format!("encode hello: {e}"))))?;
    transport
        .send(&bytes)
        .await
        .map_err(|e| SessionError::Retry(CoreError::Transport(e)))?;

    let deadline = tokio::time::Instant::now() + inner.config.connect_timeout;
    for _ in 0..5 {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let frame = match timeout(remaining, transport.recv()).await {
            Err(_) => return Err(SessionError::Retry(CoreError::NotConnected)),
            Ok(Err(error)) => return Err(SessionError::Retry(CoreError::Transport(error))),
            Ok(Ok(None)) => {
                return Err(SessionError::Retry(CoreError::Transport(
                    crate::error::TransportError::closed("closed during handshake"),
                )))
            }
            Ok(Ok(Some(bytes))) => bytes,
        };
        let parsed =
            match rchat_proto::decode_envelope(&frame, inner.config.max_frame_bytes as usize) {
                Ok(envelope) => envelope,
                Err(error) => {
                    return Err(SessionError::Fatal(CoreError::Protocol(format!(
                        "handshake decode: {error}"
                    ))))
                }
            };
        match parsed.body {
            Some(envelope::Body::ServerHello(server_hello)) => {
                validate_hello(inner, &server_hello)?;
                return Ok(server_hello);
            }
            Some(envelope::Body::Error(error)) => {
                return Err(SessionError::Fatal(rejected_from(&error)));
            }
            other => {
                debug!("ignoring envelope during handshake: {other:?}");
            }
        }
    }
    Err(SessionError::Retry(CoreError::NotConnected))
}

fn validate_hello(inner: &Inner, server_hello: &proto::ServerHello) -> Result<(), SessionError> {
    if server_hello.protocol_major != rchat_proto::PROTOCOL_MAJOR {
        return Err(SessionError::Fatal(CoreError::Rejected {
            code: Some(proto::ErrorCode::ProtocolVersionMismatch),
            raw_code: proto::ErrorCode::ProtocolVersionMismatch as i32,
            message: format!(
                "server speaks protocol major {}, client requires {}",
                server_hello.protocol_major,
                rchat_proto::PROTOCOL_MAJOR
            ),
            retryable: false,
        }));
    }
    let required = required_capabilities(inner);
    let missing: Vec<&String> = required
        .iter()
        .filter(|cap| !server_hello.capabilities.contains(cap))
        .collect();
    if !missing.is_empty() {
        return Err(SessionError::Fatal(CoreError::Rejected {
            code: Some(proto::ErrorCode::UnsupportedCapability),
            raw_code: proto::ErrorCode::UnsupportedCapability as i32,
            message: format!("server lacks required capabilities: {missing:?}"),
            retryable: false,
        }));
    }
    if inner.config.e2e_enabled {
        let expected = inner
            .crypto
            .as_ref()
            .map(|c| c.protocol_name())
            .unwrap_or(rchat_proto::E2E_PROTOCOL_OLM_V1);
        if server_hello.e2e_protocol != expected {
            return Err(SessionError::Fatal(CoreError::Rejected {
                code: Some(proto::ErrorCode::UnsupportedCapability),
                raw_code: proto::ErrorCode::UnsupportedCapability as i32,
                message: format!(
                    "server e2e protocol {:?} != required {:?}",
                    server_hello.e2e_protocol, expected
                ),
                retryable: false,
            }));
        }
    }
    Ok(())
}

#[allow(dead_code)]
fn unused_message_status(_: MessageStatus) {}

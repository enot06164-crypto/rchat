//! Публичный API клиентского ядра и его внутреннее состояние.
//!
//! Публичный API не раскрывает типы конкретного транспорта, SQL-драйвера,
//! CLI-парсера или криптографической библиотеки. Ядро использует tokio как
//! выбранный async-стек (обоснование — docs/architecture.md), но наружу
//! runtime-специфичные типы не выставляются.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use rchat_proto::proto;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::actor::actor_main;
use crate::config::ClientConfig;
use crate::convert;
use crate::error::CoreError;
use crate::events::{EventSubscription, UiEvent};
use crate::traits::{secret_keys, ClientStore, CryptoProvider, SecretStore, TransportFactory};
use crate::types::{
    ChatRecord, ConnectionState, DeviceRecord, MessageRecord, MessageStatus, OutboxRecord,
    OutboxState, SyncProgress,
};

/// Активная сессия на этом устройстве.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionInfo {
    pub user_id: String,
    pub username: String,
    pub display_name: String,
    pub device_id: String,
    pub session_expires_at_ms: u64,
}

/// Команды актору соединения.
pub(crate) enum Command {
    /// Произвольный запрос (кроме register/login/resume/logout).
    ///
    /// `method` в боксе: одном сообщении `Method` — большая oneof-обёртка,
    /// и без бокса команда канала раздувает каждое сообщение.
    Request {
        method: Box<proto::request::Method>,
        reply: oneshot::Sender<Result<proto::response::Result, CoreError>>,
    },
    Register {
        username: String,
        display_name: String,
        password: String,
        reply: oneshot::Sender<Result<(), CoreError>>,
    },
    Login {
        username: String,
        password: String,
        reply: oneshot::Sender<Result<(), CoreError>>,
    },
    SyncNow {
        reply: oneshot::Sender<Result<SyncProgress, CoreError>>,
    },
    /// История из сервера с расшифрованием и сохранением (внутри актора).
    FetchHistory {
        chat_id: String,
        before_server_id: Option<u64>,
        limit: u32,
        reply: oneshot::Sender<Result<Vec<MessageRecord>, CoreError>>,
    },
    Logout {
        wipe: bool,
        reply: oneshot::Sender<Result<(), CoreError>>,
    },
    /// Разбудить обработчик outbox (после локальной постановки сообщения).
    Kick,
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

pub(crate) struct Inner {
    pub config: ClientConfig,
    pub transport: Arc<dyn TransportFactory>,
    pub storage: Arc<dyn ClientStore>,
    pub secrets: Arc<dyn SecretStore>,
    pub crypto: Option<Arc<dyn CryptoProvider>>,
    pub events: broadcast::Sender<UiEvent>,
    pub state_tx: watch::Sender<ConnectionState>,
    pub command_tx: mpsc::Sender<Command>,
    pub command_rx: Mutex<Option<mpsc::Receiver<Command>>>,
    pub server_hello: RwLock<Option<proto::ServerHello>>,
    pub session: RwLock<Option<SessionInfo>>,
    pub device_id_cache: Mutex<Option<String>>,
    pub request_counter: AtomicU64,
    pub started: AtomicBool,
    pub shutdown: AtomicBool,
    pub actor: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Inner {
    pub(crate) fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    pub(crate) fn emit(&self, event: UiEvent) {
        // Отсутствие подписчиков не является ошибкой.
        let _ = self.events.send(event);
    }

    pub(crate) fn set_state(&self, state: ConnectionState) {
        let _ = self.state_tx.send(state);
    }

    pub(crate) fn state(&self) -> ConnectionState {
        *self.state_tx.borrow()
    }

    pub(crate) fn next_request_id(&self) -> u64 {
        self.request_counter.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub(crate) fn session_info(&self) -> Option<SessionInfo> {
        self.session.read().expect("session lock poisoned").clone()
    }

    pub(crate) fn set_session(&self, session: Option<SessionInfo>) {
        *self.session.write().expect("session lock poisoned") = session;
    }

    /// Постоянный идентификатор этого устройства. Генерируется один раз и
    /// хранится в защищённом хранилище.
    pub(crate) fn device_id(&self) -> Result<String, CoreError> {
        {
            let cache = self
                .device_id_cache
                .lock()
                .expect("device id cache poisoned");
            if let Some(id) = cache.as_ref() {
                return Ok(id.clone());
            }
        }
        let stored = self
            .secrets
            .get(secret_keys::DEVICE_ID)?
            .map(|bytes| {
                String::from_utf8(bytes).map_err(|_| CoreError::Internal("bad device id".into()))
            })
            .transpose()?;
        let id = match stored {
            Some(id) if !id.is_empty() => id,
            _ => {
                let id = rchat_proto::random_id_hex(16)
                    .map_err(|e| CoreError::Internal(format!("rng: {e}")))?;
                self.secrets.set(secret_keys::DEVICE_ID, id.as_bytes())?;
                id
            }
        };
        *self
            .device_id_cache
            .lock()
            .expect("device id cache poisoned") = Some(id.clone());
        Ok(id)
    }

    pub(crate) fn clear_device_id_cache(&self) {
        *self
            .device_id_cache
            .lock()
            .expect("device id cache poisoned") = None;
    }
}

/// Сборщик клиента: composition root передаёт конкретные реализации.
pub struct ClientBuilder {
    config: ClientConfig,
    transport: Arc<dyn TransportFactory>,
    storage: Arc<dyn ClientStore>,
    secrets: Arc<dyn SecretStore>,
    crypto: Option<Arc<dyn CryptoProvider>>,
}

impl ClientBuilder {
    pub fn new(
        config: ClientConfig,
        transport: Arc<dyn TransportFactory>,
        storage: Arc<dyn ClientStore>,
        secrets: Arc<dyn SecretStore>,
    ) -> Self {
        Self {
            config,
            transport,
            storage,
            secrets,
            crypto: None,
        }
    }

    pub fn crypto_provider(mut self, crypto: Arc<dyn CryptoProvider>) -> Self {
        self.crypto = Some(crypto);
        self
    }

    pub fn build(self) -> Result<Client, CoreError> {
        match (self.config.e2e_enabled, self.crypto.is_some()) {
            (true, false) => Err(CoreError::InvalidInput(
                "e2e enabled but no crypto provider supplied".into(),
            )),
            (false, true) => Err(CoreError::InvalidInput(
                "e2e disabled but a crypto provider was supplied; remove one of the two".into(),
            )),
            _ => Ok(()),
        }?;
        let (command_tx, command_rx) = mpsc::channel(64);
        let (events, _) = broadcast::channel(512);
        let (state_tx, _) = watch::channel(ConnectionState::Disconnected);
        let inner = Arc::new(Inner {
            config: self.config,
            transport: self.transport,
            storage: self.storage,
            secrets: self.secrets,
            crypto: self.crypto,
            events,
            state_tx,
            command_tx,
            command_rx: Mutex::new(Some(command_rx)),
            server_hello: RwLock::new(None),
            session: RwLock::new(None),
            device_id_cache: Mutex::new(None),
            request_counter: AtomicU64::new(0),
            started: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            actor: Mutex::new(None),
        });
        Ok(Client { inner })
    }
}

/// Клиент мессенджера.
#[derive(Clone)]
pub struct Client {
    pub(crate) inner: Arc<Inner>,
}

impl Client {
    /// Инициализирует хранилище и запускает фоновые задачи.
    /// Идемпотентно. Требует запущенного tokio runtime.
    pub async fn start(&self) -> Result<(), CoreError> {
        if self.inner.started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.inner.storage.migrate().await?;
        // Криптографическое состояние инициализируется заранее, чтобы ошибка
        // конфигурации проявлялась до сетевых попыток.
        if let Some(crypto) = self.inner.crypto.as_ref() {
            crypto.initialize().await?;
        }
        let rx = self
            .inner
            .command_rx
            .lock()
            .expect("command rx poisoned")
            .take()
            .ok_or_else(|| CoreError::Internal("client already started".into()))?;
        let inner = Arc::clone(&self.inner);
        let handle = tokio::spawn(async move { actor_main(inner, rx).await });
        *self.inner.actor.lock().expect("actor lock poisoned") = Some(handle);
        Ok(())
    }

    /// Останавливает клиент. Безопасно вызывать многократно.
    pub async fn shutdown(&self) -> Result<(), CoreError> {
        if !self.inner.started.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.inner.shutdown.store(true, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        if self
            .inner
            .command_tx
            .send(Command::Shutdown { reply: tx })
            .await
            .is_ok()
        {
            let _ = tokio::time::timeout(Duration::from_secs(5), rx).await;
        }
        let handle = self.inner.actor.lock().expect("actor lock poisoned").take();
        if let Some(handle) = handle {
            let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
        }
        Ok(())
    }

    pub fn subscribe(&self) -> EventSubscription {
        EventSubscription::new(self.inner.events.subscribe())
    }

    pub fn connection_state(&self) -> ConnectionState {
        self.inner.state()
    }

    pub fn session(&self) -> Option<(String, String, String)> {
        self.inner
            .session_info()
            .map(|s| (s.user_id, s.username, s.display_name))
    }

    /// Ждёт, пока транспортное соединение будет установлено
    /// (Connected или Unauthenticated).
    pub async fn wait_online(&self, timeout: Duration) -> Result<ConnectionState, CoreError> {
        let mut rx = self.inner.state_tx.subscribe();
        let deadline = Instant::now() + timeout;
        loop {
            let state = *rx.borrow();
            if matches!(
                state,
                ConnectionState::Connected | ConnectionState::Unauthenticated
            ) {
                return Ok(state);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(CoreError::NotConnected);
            }
            match tokio::time::timeout(remaining, rx.changed()).await {
                Err(_) => return Err(CoreError::NotConnected),
                Ok(Ok(())) => continue,
                Ok(Err(_)) => return Err(CoreError::Closed),
            }
        }
    }

    // -----------------------------------------------------------------
    // Регистрация и сессии
    // -----------------------------------------------------------------

    pub async fn register(
        &self,
        username: &str,
        display_name: &str,
        password: &str,
    ) -> Result<(), CoreError> {
        self.start().await?;
        self.wait_online(self.inner.config.connect_timeout).await?;
        let username = rchat_proto::validate::username(username)
            .map_err(|e| CoreError::InvalidInput(e.to_string()))?;
        let display_name = rchat_proto::validate::display_name(display_name)
            .map_err(|e| CoreError::InvalidInput(e.to_string()))?;
        rchat_proto::validate::password(password)
            .map_err(|e| CoreError::InvalidInput(e.to_string()))?;
        let (tx, rx) = oneshot::channel();
        self.send(Command::Register {
            username,
            display_name,
            password: password.to_string(),
            reply: tx,
        })
        .await?;
        self.await_reply(rx).await?
    }

    pub async fn login(&self, username: &str, password: &str) -> Result<(), CoreError> {
        self.start().await?;
        self.wait_online(self.inner.config.connect_timeout).await?;
        let username = rchat_proto::validate::username(username)
            .map_err(|e| CoreError::InvalidInput(e.to_string()))?;
        rchat_proto::validate::password(password)
            .map_err(|e| CoreError::InvalidInput(e.to_string()))?;
        let (tx, rx) = oneshot::channel();
        self.send(Command::Login {
            username,
            password: password.to_string(),
            reply: tx,
        })
        .await?;
        self.await_reply(rx).await?
    }

    /// Выход: отзывает серверную сессию (best-effort), удаляет локальный токен.
    /// `wipe=true` дополнительно удаляет локальные данные и E2E-состояние.
    pub async fn logout(&self, wipe: bool) -> Result<(), CoreError> {
        self.start().await?;
        let (tx, rx) = oneshot::channel();
        self.send(Command::Logout { wipe, reply: tx }).await?;
        self.await_reply(rx).await?
    }

    // -----------------------------------------------------------------
    // Чаты
    // -----------------------------------------------------------------

    pub async fn list_chats(&self) -> Result<Vec<ChatRecord>, CoreError> {
        let resp = self
            .request(proto::request::Method::ListChats(
                proto::ListChatsRequest {},
            ))
            .await?;
        let chats = match resp {
            proto::response::Result::ListChats(r) => r.chats,
            other => return Err(unexpected(other)),
        };
        let session = self.require_session()?;
        let mut records = Vec::with_capacity(chats.len());
        for chat in &chats {
            let (record, members) = convert::chat_from_proto(chat, &session.user_id)?;
            self.inner.storage.upsert_chat(&record, &members).await?;
            self.inner.emit(UiEvent::ChatUpserted(record.clone()));
            records.push(record);
        }
        Ok(records)
    }

    pub async fn create_direct_chat(
        &self,
        peer_username: &str,
        e2e: bool,
    ) -> Result<ChatRecord, CoreError> {
        if e2e && self.inner.crypto.is_none() {
            return Err(CoreError::E2eUnavailable(
                "e2e chat requested but no crypto provider configured".into(),
            ));
        }
        let username = rchat_proto::validate::username(peer_username)
            .map_err(|e| CoreError::InvalidInput(e.to_string()))?;
        let mode = if e2e {
            proto::E2eMode::OlmV1
        } else {
            proto::E2eMode::NoneDev
        };
        let resp = self
            .request(proto::request::Method::CreateDirectChat(
                proto::CreateDirectChatRequest {
                    peer_username: username,
                    e2e_mode: mode as i32,
                },
            ))
            .await?;
        let chat = match resp {
            proto::response::Result::CreateDirectChat(r) => r
                .chat
                .ok_or_else(|| CoreError::Protocol("create chat response without chat".into()))?,
            other => return Err(unexpected(other)),
        };
        let session = self.require_session()?;
        let (record, members) = convert::chat_from_proto(&chat, &session.user_id)?;
        self.inner.storage.upsert_chat(&record, &members).await?;
        self.inner.emit(UiEvent::ChatUpserted(record.clone()));
        // Кешируем устройства участников, чтобы отправка работала и офлайн.
        let user_ids: Vec<String> = members.iter().map(|m| m.user_id.clone()).collect();
        let _ = self.refresh_devices(&user_ids).await;
        Ok(record)
    }

    /// Обновляет кеш устройств указанных пользователей с сервера (best-effort).
    pub async fn refresh_devices(&self, user_ids: &[String]) -> Result<(), CoreError> {
        if user_ids.is_empty() {
            return Ok(());
        }
        let resp = self
            .request(proto::request::Method::GetDevices(
                proto::GetDevicesRequest {
                    user_ids: user_ids.to_vec(),
                    include_own: true,
                },
            ))
            .await?;
        let devices = match resp {
            proto::response::Result::GetDevices(r) => r.devices,
            other => return Err(unexpected(other)),
        };
        for device in &devices {
            let record = convert::device_from_proto(device)?;
            if record.user_id.is_empty() {
                continue; // сервер не приложил владельца — пропускаем кеширование
            }
            self.inner.storage.upsert_device(&record).await?;
        }
        Ok(())
    }

    /// Устройства собственного аккаунта (с сервера, с кешированием).
    pub async fn devices(&self) -> Result<Vec<DeviceRecord>, CoreError> {
        let resp = self
            .request(proto::request::Method::ListDevices(
                proto::ListDevicesRequest {},
            ))
            .await?;
        let devices = match resp {
            proto::response::Result::ListDevices(r) => r.devices,
            other => return Err(unexpected(other)),
        };
        let _session = self.require_session()?;
        let mut records = Vec::with_capacity(devices.len());
        for device in &devices {
            let record = convert::device_from_proto(device)?;
            self.inner.storage.upsert_device(&record).await?;
            records.push(record);
        }
        Ok(records)
    }

    /// Отзывает устройство аккаунта. Собственное устройство отозвать можно:
    /// сервер отключит его соединения и аннулирует авторизацию (для выхода
    /// из аккаунта используйте `logout`).
    pub async fn revoke_device(&self, device_id: &str) -> Result<(), CoreError> {
        let resp = self
            .request(proto::request::Method::RevokeDevice(
                proto::RevokeDeviceRequest {
                    device_id: device_id.to_string(),
                },
            ))
            .await?;
        match resp {
            proto::response::Result::RevokeDevice(_) => {}
            other => return Err(unexpected(other)),
        }
        if let Some(crypto) = self.inner.crypto.as_ref() {
            crypto.remove_session(device_id).await?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------
    // Сообщения
    // -----------------------------------------------------------------

    /// Ставит текстовое сообщение в локальную очередь отправки.
    ///
    /// Для E2E-чата шифрование выполняется сразу; если сессия с каким-то
    /// устройством не установлена, требуется сеть для получения prekey.
    /// Открытый текст для E2E-чата не отправляется никогда.
    pub async fn send_text(
        &self,
        chat_id: &str,
        text: &str,
        reply_to: Option<u64>,
    ) -> Result<String, CoreError> {
        self.start().await?;
        if text.is_empty() {
            return Err(CoreError::InvalidInput("empty message".into()));
        }
        rchat_proto::validate::chat_id(chat_id)
            .map_err(|e| CoreError::InvalidInput(e.to_string()))?;
        let chat = self
            .inner
            .storage
            .get_chat(chat_id)
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("chat {chat_id}")))?;
        let account = self
            .inner
            .storage
            .load_account()
            .await?
            .ok_or(CoreError::NotAuthenticated)?;
        let device_id = self.inner.device_id()?;

        // Перед шифрованием обновляем кэш устройств участников: набор мог
        // измениться (новое устройство или отзыв). Без сети продолжаем с
        // кэшем — отправка при наличии установленных сессий возможна офлайн.
        if chat.e2e_mode == proto::E2eMode::OlmV1 {
            let members = self.inner.storage.chat_members(chat_id).await?;
            let user_ids: Vec<String> = members.iter().map(|m| m.user_id.clone()).collect();
            let _ = self.refresh_devices(&user_ids).await;
        }

        let local_id = convert::new_local_message_id()?;
        let client = self.clone();
        let fetch = move |ids: Vec<String>| async move { client.fetch_bundles(&ids).await };
        let request = crate::send::compose_send_request(
            &self.inner,
            &local_id,
            fetch,
            &chat,
            &account,
            text,
            reply_to,
        )
        .await?;
        let mut request_bytes = Vec::new();
        prost::Message::encode(&request, &mut request_bytes)
            .map_err(|e| CoreError::Internal(format!("encode: {e}")))?;

        let now = rchat_proto::now_ms();
        let message = MessageRecord {
            local_id: local_id.clone(),
            server_id: None,
            chat_id: chat_id.to_string(),
            sender_user_id: account.user_id.clone(),
            sender_device_id: device_id.clone(),
            content_kind: "text".to_string(),
            body: Some(text.to_string()),
            reply_to_server_id: reply_to,
            status: Some(MessageStatus::Pending),
            created_at_ms: now,
            edited_at_ms: None,
            deleted_at_ms: None,
        };
        let outbox = OutboxRecord {
            local_id: local_id.clone(),
            chat_id: chat_id.to_string(),
            idempotency_key: local_id.clone(),
            request: request_bytes,
            state: OutboxState::Pending,
            attempts: 0,
            next_attempt_at_ms: 0,
            last_error_code: None,
            last_error: None,
            created_at_ms: now,
            server_message_id: None,
        };
        self.inner
            .storage
            .insert_outgoing(&message, &outbox)
            .await?;
        self.inner.emit(UiEvent::OutboxChanged {
            local_id: local_id.clone(),
            state: OutboxState::Pending,
            error: None,
        });
        // Разбудить обработчик outbox; при переполнении канала тик сделает это сам.
        let _ = self.inner.command_tx.try_send(Command::Kick);
        Ok(local_id)
    }

    /// Загружает историю с сервера, сохраняет локально (идемпотентно) и
    /// возвращает записи. Для E2E текст расшифровывается внутри актора.
    pub async fn fetch_history(
        &self,
        chat_id: &str,
        before_server_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<MessageRecord>, CoreError> {
        self.start().await?;
        self.wait_online(self.inner.config.connect_timeout).await?;
        let (tx, rx) = oneshot::channel();
        self.send(Command::FetchHistory {
            chat_id: chat_id.to_string(),
            before_server_id,
            limit,
            reply: tx,
        })
        .await?;
        self.await_reply(rx).await?
    }

    /// Локальная история из хранилища (без обращения к серверу).
    pub async fn list_messages(
        &self,
        chat_id: &str,
        before_server_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<MessageRecord>, CoreError> {
        Ok(self
            .inner
            .storage
            .list_messages(chat_id, before_server_id, limit)
            .await?)
    }

    pub async fn mark_read(&self, chat_id: &str, up_to_server_id: u64) -> Result<(), CoreError> {
        let resp = self
            .request(proto::request::Method::MarkReceipt(
                proto::MarkReceiptRequest {
                    chat_id: chat_id.to_string(),
                    up_to_server_id,
                    read: true,
                },
            ))
            .await?;
        match resp {
            proto::response::Result::MarkReceipt(_) => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    /// Явный запуск синхронизации (диагностика/тесты).
    pub async fn sync_now(&self) -> Result<SyncProgress, CoreError> {
        self.start().await?;
        self.wait_online(self.inner.config.connect_timeout).await?;
        let (tx, rx) = oneshot::channel();
        self.send(Command::SyncNow { reply: tx }).await?;
        self.await_reply(rx).await?
    }

    // -----------------------------------------------------------------
    // Внутренние помощники
    // -----------------------------------------------------------------

    async fn send(&self, command: Command) -> Result<(), CoreError> {
        self.inner
            .command_tx
            .send(command)
            .await
            .map_err(|_| CoreError::Closed)
    }

    async fn await_reply<T>(&self, rx: oneshot::Receiver<T>) -> Result<T, CoreError> {
        match tokio::time::timeout(self.inner.config.request_timeout, rx).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => Err(CoreError::Closed),
            Err(_) => Err(CoreError::Timeout),
        }
    }

    async fn await_reply_any(
        &self,
        rx: oneshot::Receiver<Result<proto::response::Result, CoreError>>,
    ) -> Result<proto::response::Result, CoreError> {
        match tokio::time::timeout(self.inner.config.request_timeout, rx).await {
            Ok(Ok(Ok(resp))) => Ok(resp),
            Ok(Ok(Err(err))) => Err(err),
            Ok(Err(_)) => Err(CoreError::Closed),
            Err(_) => Err(CoreError::Timeout),
        }
    }

    /// Отправляет запрос через актор и ждёт ответ.
    pub(crate) async fn request(
        &self,
        method: proto::request::Method,
    ) -> Result<proto::response::Result, CoreError> {
        self.start().await?;
        self.wait_online(self.inner.config.connect_timeout).await?;
        if self.inner.session_info().is_none() {
            return Err(CoreError::NotAuthenticated);
        }
        let (tx, rx) = oneshot::channel();
        self.inner
            .command_tx
            .send(Command::Request {
                method: Box::new(method),
                reply: tx,
            })
            .await
            .map_err(|_| CoreError::Closed)?;
        self.await_reply_any(rx).await
    }

    fn require_session(&self) -> Result<SessionInfo, CoreError> {
        self.inner.session_info().ok_or(CoreError::NotAuthenticated)
    }

    async fn fetch_bundles(
        &self,
        device_ids: &[String],
    ) -> Result<Vec<crate::traits::PeerDevice>, CoreError> {
        let resp = self
            .request(proto::request::Method::GetDeviceBundles(
                proto::GetDeviceBundlesRequest {
                    device_ids: device_ids.to_vec(),
                },
            ))
            .await?;
        let bundles = match resp {
            proto::response::Result::GetDeviceBundles(r) => r.bundles,
            other => return Err(unexpected(other)),
        };
        let mut out = Vec::with_capacity(bundles.len());
        for bundle in &bundles {
            let keys = bundle.keys.clone().unwrap_or_default();
            out.push(crate::traits::PeerDevice {
                user_id: bundle.user_id.clone(),
                device_id: bundle.device_id.clone(),
                ed25519: keys.ed25519,
                curve25519: keys.curve25519,
                one_time_key: bundle
                    .one_time_key
                    .as_ref()
                    .map(|k| crate::traits::OneTimeKey::from_proto(k, bundle.fallback)),
            });
        }
        Ok(out)
    }
}

pub(crate) fn unexpected(response: proto::response::Result) -> CoreError {
    match response {
        proto::response::Result::Error(error) => crate::error::rejected_from(&error),
        other => CoreError::Protocol(format!("unexpected response: {other:?}")),
    }
}

/// Состояние, разделяемое актором между итерациями цикла соединения.
pub(crate) struct LoopState {
    pub authenticated: bool,
    pub needs_bootstrap: bool,
    pub needs_sync: bool,
    pub malformed_frames: u32,
    pub outbox_paused: bool,
    pub pending_receipts: BTreeMap<String, u64>,
    pub device_refresh: std::collections::BTreeSet<String>,
}

impl LoopState {
    pub(crate) fn new() -> Self {
        Self {
            authenticated: false,
            needs_bootstrap: false,
            needs_sync: false,
            malformed_frames: 0,
            outbox_paused: false,
            pending_receipts: BTreeMap::new(),
            device_refresh: std::collections::BTreeSet::new(),
        }
    }
}

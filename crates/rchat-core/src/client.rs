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
use tracing::warn;

use crate::actor::actor_main;
use crate::config::ClientConfig;
use crate::convert;
use crate::error::CoreError;
use crate::events::{EventSubscription, UiEvent};
use crate::payload::{Attachment, Payload};
use crate::traits::{secret_keys, ClientStore, CryptoProvider, SecretStore, TransportFactory};
use crate::types::{
    CallRecord, ChatMode, ChatRecord, ConnectionState, DeviceRecord, MessageRecord, MessageStatus,
    OutboxRecord, OutboxState, ReceiptRecord, SyncApply, SyncProgress,
};

/// Короткое окно ожидания сети перед сборкой сообщения: клиент, только что
/// запущенный (например, CLI — отдельный процесс на команду), успевает
/// догнать журнал; офлайн-отправка при этом не блокируется надолго.
const PRE_COMPOSE_CONNECT_WAIT: Duration = Duration::from_secs(2);

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

/// Размер части файла при загрузке/скачивании блоба (байт).
const BLOB_CHUNK_BYTES: usize = 64 * 1024;

/// Предел локального кэша скачанных файлов (байт). При переполнении
/// вытесняются самые старые кэшированные вложения.
const MAX_ATTACHMENT_CACHE_BYTES: u64 = 512 * 1024 * 1024;

/// Случайный hex-идентификатор (для blob_id).
fn random_hex(bytes: usize) -> Result<String, CoreError> {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).map_err(|e| CoreError::Internal(format!("random id: {e}")))?;
    Ok(buffer.iter().map(|b| format!("{b:02x}")).collect())
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

    /// Реакция на изменение состава группового E2E-чата: если кто-то выбыл из
    /// чата, исходящая megolm-сессия ротируется — выбывший не должен читать
    /// новые сообщения (ключ новой сессии ему не выдаётся). Вызывается до
    /// применения нового состава, чтобы сравнить его с сохранённым.
    pub(crate) async fn note_group_membership(
        &self,
        chat: &ChatRecord,
        new_members: &[crate::types::MemberRecord],
    ) {
        if chat.kind != proto::ChatKind::Group || chat.e2e_mode != proto::E2eMode::OlmV1 {
            return;
        }
        let old = match self.storage.chat_members(&chat.chat_id).await {
            Ok(members) => members,
            Err(error) => {
                warn!(
                    error = %error,
                    chat_id = %chat.chat_id,
                    "failed to read members before rotation"
                );
                return;
            }
        };
        // Первый снимок чата (локально состав пуст) — не изменение.
        if old.is_empty() {
            return;
        }
        let removed = old
            .iter()
            .any(|member| !new_members.iter().any(|m| m.user_id == member.user_id));
        if removed {
            self.rotate_group_session(&chat.chat_id, "member removed")
                .await;
        }
    }

    /// Ротация исходящей групповой сессии чата: следующая отправка создаст
    /// новую сессию и выдаст её актуальному составу.
    pub(crate) async fn rotate_group_session(&self, chat_id: &str, reason: &str) {
        if let Some(crypto) = self.crypto.as_ref() {
            if let Err(error) = crypto.rotate_group_session(chat_id).await {
                warn!(
                    error = %error,
                    chat_id = %chat_id,
                    reason,
                    "failed to rotate group session"
                );
            }
        }
        if let Err(error) = self.storage.clear_group_key_shares(chat_id).await {
            warn!(
                error = %error,
                chat_id = %chat_id,
                reason,
                "failed to clear group key share records"
            );
        }
    }

    pub(crate) fn emit(&self, event: UiEvent) {
        // Отсутствие подписчиков не является ошибкой.
        let _ = self.events.send(event);
    }

    pub(crate) fn set_state(&self, state: ConnectionState) {
        // `send_replace`, а не `send`: смена состояния обязана сохраняться и
        // при отсутствии подписчиков, иначе клиент, успевший подключиться до
        // первой подписки, навсегда остаётся для наблюдателей «отключённым».
        self.state_tx.send_replace(state);
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

    /// Ключ аккаунта для облачных файлов: им клиент шифрует файл перед
    /// загрузкой, а сервер знает ключ и может хранить файл и отдавать его
    /// участникам чата (как DC в облачных чатах Telegram).  Ключ принадлежит
    /// аккаунту: новое устройство получает его от сервера.
    pub(crate) async fn cloud_account_key(&self) -> Result<[u8; 32], CoreError> {
        if let Some(stored) = self.secrets.get(secret_keys::CLOUD_ACCOUNT_KEY)? {
            if stored.len() == 32 {
                let mut key = [0u8; 32];
                key.copy_from_slice(&stored);
                return Ok(key);
            }
            warn!("stored cloud account key has wrong length; regenerating");
        }
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).map_err(|e| CoreError::Internal(format!("rng: {e}")))?;
        self.secrets.set(secret_keys::CLOUD_ACCOUNT_KEY, &key)?;
        Ok(key)
    }

    /// Сохраняет ключ аккаунта, полученный от сервера (сервер — источник
    /// истины: если ключ уже был, сервер отдаёт именно его).
    pub(crate) fn store_cloud_account_key(&self, key: &[u8]) -> Result<(), CoreError> {
        self.secrets.set(secret_keys::CLOUD_ACCOUNT_KEY, key)?;
        Ok(())
    }

    /// Режим чатов по умолчанию (облачный или приватный). Хранится в
    /// профиле устройства; по умолчанию — облачный.
    pub(crate) fn default_chat_mode(&self) -> ChatMode {
        self.secrets
            .get(secret_keys::DEFAULT_CHAT_MODE)
            .ok()
            .flatten()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|value| ChatMode::parse(value.trim()))
            .unwrap_or(ChatMode::DEFAULT)
    }

    /// Меняет режим чатов по умолчанию.
    pub(crate) fn set_default_chat_mode(&self, mode: ChatMode) -> Result<(), CoreError> {
        self.secrets
            .set(secret_keys::DEFAULT_CHAT_MODE, mode.as_str().as_bytes())?;
        Ok(())
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

/// Метаданные загружаемого файла (облачный режим): серверу нужны, чтобы
/// дедуплицировать файл по содержимому и показать его в списке медиа.
#[derive(Debug, Clone)]
struct FileMeta {
    name: String,
    kind: String,
    mime: String,
    plaintext_size: u64,
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

    /// Догоняет журнал перед сборкой исходящего содержимого (best-effort):
    /// состав участников и кэш устройств могли измениться, пока клиент не
    /// работал (в CLI каждая команда — отдельный запуск). Ожидание сети
    /// ограничено коротким окном: офлайн-отправка сразу ставится в очередь.
    async fn refresh_before_compose(&self) {
        if self.connection_state() != ConnectionState::Connected {
            let _ = self.wait_online(PRE_COMPOSE_CONNECT_WAIT).await;
        }
        if self.connection_state() == ConnectionState::Connected {
            if let Err(error) = self.sync_now().await {
                warn!(error = %error, "sync before compose skipped");
            }
        }
    }

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

    /// Режим чатов по умолчанию: облачный (`ChatMode::Cloud`) или приватный
    /// (`ChatMode::Private`). По умолчанию — облачный, как в обычных чатах
    /// Telegram; меняется в настройках клиента.
    pub fn default_chat_mode(&self) -> ChatMode {
        self.inner.default_chat_mode()
    }

    /// Меняет режим чатов по умолчанию (применяется к новым чатам).
    pub async fn set_default_chat_mode(&self, mode: ChatMode) -> Result<(), CoreError> {
        self.inner.set_default_chat_mode(mode)
    }

    /// Проверяет, что для выбранного режима есть всё необходимое: приватному
    /// чату нужен крипто-провайдер, облачному — тоже (ключ аккаунта шифрует
    /// содержимое симметрично).
    fn require_crypto_for(&self, mode: ChatMode) -> Result<(), CoreError> {
        match mode {
            ChatMode::Cloud => {
                if self.inner.crypto.is_none() {
                    return Err(CoreError::E2eUnavailable(
                        "cloud chat requested but no crypto provider configured".into(),
                    ));
                }
                Ok(())
            }
            ChatMode::Private => {
                if self.inner.crypto.is_none() {
                    return Err(CoreError::E2eUnavailable(
                        "e2e chat requested but no crypto provider configured".into(),
                    ));
                }
                Ok(())
            }
            ChatMode::NoneDev => Ok(()),
        }
    }

    /// Ищет по сообщениям аккаунта. Работает в облачном режиме: сервер
    /// расшифровывает содержимое ключом аккаунта и ищет по нему. В приватных
    /// чатах поиск по содержимому недоступен — сервер их не видит.
    pub async fn search_messages(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<Vec<proto::SearchHit>, CoreError> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let resp = self
            .request(proto::request::Method::SearchMessages(
                proto::SearchMessagesRequest {
                    query: query.to_string(),
                    limit,
                },
            ))
            .await?;
        match resp {
            proto::response::Result::SearchMessages(r) => {
                // Сервер отдаёт расшифрованный конверт содержимого; текст
                // разбираем здесь (формат конверта известен клиенту) и
                // оставляем только действительные совпадения.
                let needle = query.to_lowercase();
                let mut hits = Vec::new();
                for mut hit in r.hits {
                    let payload = crate::payload::Payload::decode(hit.body.as_bytes());
                    if !payload.text.to_lowercase().contains(&needle) {
                        continue;
                    }
                    hit.body = payload.text;
                    hits.push(hit);
                }
                Ok(hits)
            }
            other => Err(unexpected(other)),
        }
    }

    /// Создаёт личный чат в заданном режиме: облачный (сервер знает ключ
    /// аккаунта и умеет искать по истории) или приватный (E2E).
    pub async fn create_direct_chat(
        &self,
        peer_username: &str,
        mode: ChatMode,
    ) -> Result<ChatRecord, CoreError> {
        self.require_crypto_for(mode)?;
        let username = rchat_proto::validate::username(peer_username)
            .map_err(|e| CoreError::InvalidInput(e.to_string()))?;
        let resp = self
            .request(proto::request::Method::CreateDirectChat(
                proto::CreateDirectChatRequest {
                    peer_username: username,
                    e2e_mode: mode.proto_mode() as i32,
                },
            ))
            .await?;
        let chat = match resp {
            proto::response::Result::CreateDirectChat(r) => r
                .chat
                .ok_or_else(|| CoreError::Protocol("create chat response without chat".into()))?,
            other => return Err(unexpected(other)),
        };
        self.store_chat_snapshot(&chat).await
    }

    /// Создаёт групповой чат в заданном режиме. Создатель становится
    /// владельцем; приглашённые получают немедленное членство. Для приватной
    /// группы все устройства участников обязаны иметь ключи (сервер это
    /// проверяет).
    pub async fn create_group_chat(
        &self,
        name: &str,
        member_usernames: &[String],
        mode: ChatMode,
    ) -> Result<ChatRecord, CoreError> {
        self.require_crypto_for(mode)?;
        let name = rchat_proto::validate::group_name(name)
            .map_err(|e| CoreError::InvalidInput(e.to_string()))?;
        let mut usernames = Vec::with_capacity(member_usernames.len());
        for username in member_usernames {
            usernames.push(
                rchat_proto::validate::username(username)
                    .map_err(|e| CoreError::InvalidInput(e.to_string()))?,
            );
        }
        let resp = self
            .request(proto::request::Method::CreateGroupChat(
                proto::CreateGroupChatRequest {
                    name,
                    member_usernames: usernames,
                    e2e_mode: mode.proto_mode() as i32,
                },
            ))
            .await?;
        let chat = match resp {
            proto::response::Result::CreateGroupChat(r) => r
                .chat
                .ok_or_else(|| CoreError::Protocol("create group response without chat".into()))?,
            other => return Err(unexpected(other)),
        };
        self.store_chat_snapshot(&chat).await
    }

    /// Приглашает пользователей в групповой чат (любой участник). Повторное
    /// приглашение уже состоящих идемпотентно. Для E2E-группы у приглашаемых
    /// должны быть устройства с ключами.
    pub async fn invite_to_chat(
        &self,
        chat_id: &str,
        usernames: &[String],
    ) -> Result<ChatRecord, CoreError> {
        rchat_proto::validate::chat_id(chat_id)
            .map_err(|e| CoreError::InvalidInput(e.to_string()))?;
        if usernames.is_empty() {
            return Err(CoreError::InvalidInput("no usernames to invite".into()));
        }
        let mut normalized = Vec::with_capacity(usernames.len());
        for username in usernames {
            normalized.push(
                rchat_proto::validate::username(username)
                    .map_err(|e| CoreError::InvalidInput(e.to_string()))?,
            );
        }
        let resp = self
            .request(proto::request::Method::InviteToChat(
                proto::InviteToChatRequest {
                    chat_id: chat_id.to_string(),
                    usernames: normalized,
                },
            ))
            .await?;
        let chat = match resp {
            proto::response::Result::InviteToChat(r) => r
                .chat
                .ok_or_else(|| CoreError::Protocol("invite response without chat".into()))?,
            other => return Err(unexpected(other)),
        };
        self.store_chat_snapshot(&chat).await
    }

    /// Выход из группового чата. Владелец при выходе передаёт владение
    /// старейшему участнику. Обновлённый состав приходит событием журнала.
    pub async fn leave_chat(&self, chat_id: &str) -> Result<(), CoreError> {
        rchat_proto::validate::chat_id(chat_id)
            .map_err(|e| CoreError::InvalidInput(e.to_string()))?;
        let resp = self
            .request(proto::request::Method::LeaveChat(proto::LeaveChatRequest {
                chat_id: chat_id.to_string(),
            }))
            .await?;
        match resp {
            proto::response::Result::LeaveChat(_) => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    /// Исключение участника владельцем.
    pub async fn remove_chat_member(
        &self,
        chat_id: &str,
        user_id: &str,
    ) -> Result<ChatRecord, CoreError> {
        rchat_proto::validate::chat_id(chat_id)
            .map_err(|e| CoreError::InvalidInput(e.to_string()))?;
        rchat_proto::validate::user_id(user_id)
            .map_err(|e| CoreError::InvalidInput(e.to_string()))?;
        let resp = self
            .request(proto::request::Method::RemoveChatMember(
                proto::RemoveChatMemberRequest {
                    chat_id: chat_id.to_string(),
                    user_id: user_id.to_string(),
                },
            ))
            .await?;
        let chat = match resp {
            proto::response::Result::RemoveChatMember(r) => r
                .chat
                .ok_or_else(|| CoreError::Protocol("remove member response without chat".into()))?,
            other => return Err(unexpected(other)),
        };
        self.store_chat_snapshot(&chat).await
    }

    /// Переименование группового чата владельцем.
    pub async fn rename_chat(&self, chat_id: &str, name: &str) -> Result<ChatRecord, CoreError> {
        rchat_proto::validate::chat_id(chat_id)
            .map_err(|e| CoreError::InvalidInput(e.to_string()))?;
        let name = rchat_proto::validate::group_name(name)
            .map_err(|e| CoreError::InvalidInput(e.to_string()))?;
        let resp = self
            .request(proto::request::Method::RenameChat(
                proto::RenameChatRequest {
                    chat_id: chat_id.to_string(),
                    name,
                },
            ))
            .await?;
        let chat = match resp {
            proto::response::Result::RenameChat(r) => r
                .chat
                .ok_or_else(|| CoreError::Protocol("rename response without chat".into()))?,
            other => return Err(unexpected(other)),
        };
        self.store_chat_snapshot(&chat).await
    }

    /// Участники чата из локального хранилища (состав обновляется событиями
    /// журнала и ответами сервера).
    pub async fn chat_members(
        &self,
        chat_id: &str,
    ) -> Result<Vec<crate::types::MemberRecord>, CoreError> {
        Ok(self.inner.storage.chat_members(chat_id).await?)
    }

    /// Приглашает собеседника на звонок в личном чате (только сигнализация,
    /// медиа нет). Приглашение возможно, только если собеседник в сети;
    /// состояние звонка живёт на сервере и не журналируется.
    pub async fn call_invite(&self, chat_id: &str) -> Result<CallRecord, CoreError> {
        rchat_proto::validate::chat_id(chat_id)
            .map_err(|e| CoreError::InvalidInput(e.to_string()))?;
        let resp = self
            .request(proto::request::Method::CallInvite(
                proto::CallInviteRequest {
                    chat_id: chat_id.to_string(),
                },
            ))
            .await?;
        match resp {
            proto::response::Result::CallInvite(r) => call_record(r.call),
            other => Err(unexpected(other)),
        }
    }

    /// Принимает входящий звонок (только callee).
    pub async fn call_accept(&self, call_id: &str) -> Result<CallRecord, CoreError> {
        validate_call_id(call_id)?;
        let resp = self
            .request(proto::request::Method::CallAccept(
                proto::CallAcceptRequest {
                    call_id: call_id.to_string(),
                },
            ))
            .await?;
        match resp {
            proto::response::Result::CallAccept(r) => call_record(r.call),
            other => Err(unexpected(other)),
        }
    }

    /// Отклоняет входящий ringing-звонок (только callee).
    pub async fn call_decline(&self, call_id: &str) -> Result<CallRecord, CoreError> {
        validate_call_id(call_id)?;
        let resp = self
            .request(proto::request::Method::CallDecline(
                proto::CallDeclineRequest {
                    call_id: call_id.to_string(),
                },
            ))
            .await?;
        match resp {
            proto::response::Result::CallDecline(r) => call_record(r.call),
            other => Err(unexpected(other)),
        }
    }

    /// Завершает звонок (любой участник: caller или callee).
    pub async fn call_end(&self, call_id: &str) -> Result<CallRecord, CoreError> {
        validate_call_id(call_id)?;
        let resp = self
            .request(proto::request::Method::CallEnd(proto::CallEndRequest {
                call_id: call_id.to_string(),
            }))
            .await?;
        match resp {
            proto::response::Result::CallEnd(r) => call_record(r.call),
            other => Err(unexpected(other)),
        }
    }

    /// Сохраняет снимок чата из ответа сервера и обновляет кеш устройств.
    async fn store_chat_snapshot(&self, chat: &proto::Chat) -> Result<ChatRecord, CoreError> {
        let session = self.require_session()?;
        let (record, members) = convert::chat_from_proto(chat, &session.user_id)?;
        self.inner.note_group_membership(&record, &members).await;
        self.inner.storage.upsert_chat(&record, &members).await?;
        self.inner.emit(UiEvent::ChatUpserted(record.clone()));
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
        if text.is_empty() {
            return Err(CoreError::InvalidInput("empty message".into()));
        }
        self.send_payload(chat_id, Payload::text(text), reply_to, None)
            .await
    }

    /// Отправляет сообщение с вложением (файл, фото, видео, голос).
    ///
    /// Вложение едет внутри зашифрованного payload: сервер видит только
    /// ciphertext, получатель расшифровывает конверт целиком. Текст может
    /// быть пустым (сообщение только с вложением) — но не одновременно
    /// пустым текстом и пустым вложением.
    pub async fn send_attachment(
        &self,
        chat_id: &str,
        text: &str,
        attachment: Attachment,
        reply_to: Option<u64>,
    ) -> Result<String, CoreError> {
        self.send_attachment_in_group(chat_id, text, attachment, reply_to, None)
            .await
    }

    /// Отправка вложения; `grouped_id` заполняется только для альбомов.
    async fn send_attachment_in_group(
        &self,
        chat_id: &str,
        text: &str,
        attachment: Attachment,
        reply_to: Option<u64>,
        grouped_id: Option<&str>,
    ) -> Result<String, CoreError> {
        attachment.validate().map_err(CoreError::InvalidInput)?;
        if text.is_empty() && attachment.data.is_empty() {
            return Err(CoreError::InvalidInput("empty message".into()));
        }
        // Режим чата определяет, как уходит файл.  В приватном чате клиент
        // шифрует файл случайным ключом, сервер видит только шифротекст, а
        // ключ едет внутри сообщения.  В облачном чате файл шифруется ключом
        // аккаунта, который знает сервер: он хранит файл у себя (как DC в
        // облачных чатах Telegram), дедуплицирует по содержимому и отдаёт
        // участникам чата.
        let chat = self
            .inner
            .storage
            .get_chat(chat_id)
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("chat {chat_id} not found")))?;
        let cloud = chat.e2e_mode == proto::E2eMode::Cloud;
        let meta = FileMeta {
            name: attachment.name.clone(),
            kind: attachment.kind.as_str().to_string(),
            mime: attachment.mime.clone(),
            plaintext_size: attachment.size,
        };
        let (blob_ref, key, nonce) = self
            .upload_blob(chat_id, &attachment.data, cloud, Some(&meta))
            .await?;
        // Постер видео — отдельный блоб (как thumbnail в Telegram).
        let thumb = match attachment.thumb.as_ref() {
            Some(thumb) if !thumb.data.is_empty() => {
                let thumb_meta = FileMeta {
                    // У постера своего имени нет — это кадр видео.
                    name: "poster".to_string(),
                    kind: "photo".to_string(),
                    mime: thumb.mime.clone(),
                    plaintext_size: thumb.size,
                };
                let (reference, thumb_key, thumb_nonce) = self
                    .upload_blob(chat_id, &thumb.data, cloud, Some(&thumb_meta))
                    .await?;
                Some(crate::payload::BlobRef {
                    reference,
                    key: thumb_key,
                    nonce: thumb_nonce,
                    size: thumb.data.len() as u64,
                    mime: thumb.mime.clone(),
                    data: Vec::new(),
                })
            }
            _ => attachment.thumb.clone(),
        };
        let payload = Payload::with_attachment(
            text,
            Attachment {
                blob_ref: Some(blob_ref),
                key: if key.is_empty() { None } else { Some(key) },
                nonce: if nonce.is_empty() { None } else { Some(nonce) },
                thumb,
                data: Vec::new(),
                ..attachment
            },
        );
        self.send_payload(chat_id, payload, reply_to, grouped_id)
            .await
    }

    /// Отправляет альбом (медиагруппу): несколько вложений одним набором.
    ///
    /// Вложения уходят отдельными сообщениями с общим `grouped_id` — так же,
    /// как альбомы в Telegram, где медиа одного альбома объединены.
    /// Подпись прикрепляется к последнему элементу.
    pub async fn send_album(
        &self,
        chat_id: &str,
        text: &str,
        attachments: Vec<Attachment>,
        reply_to: Option<u64>,
    ) -> Result<Vec<String>, CoreError> {
        if attachments.is_empty() {
            return Err(CoreError::InvalidInput("empty album".into()));
        }
        let grouped_id = random_hex(8)?;
        let count = attachments.len();
        let mut local_ids = Vec::with_capacity(count);
        for (index, attachment) in attachments.into_iter().enumerate() {
            let is_last = index + 1 == count;
            let caption = if is_last { text } else { "" };
            let local_id = self
                .send_attachment_in_group(chat_id, caption, attachment, reply_to, Some(&grouped_id))
                .await?;
            local_ids.push(local_id);
        }
        Ok(local_ids)
    }

    /// Содержимое вложения сообщения: из локального кэша или скачиванием
    /// блоба с расшифровкой.
    pub async fn attachment_content(
        &self,
        server_id: u64,
    ) -> Result<Option<Attachment>, CoreError> {
        let Some(message) = self.message_by_server_id(server_id).await? else {
            return Ok(None);
        };
        let Some(attachment) = message.attachment else {
            return Ok(None);
        };
        if !attachment.data.is_empty() {
            return Ok(Some(attachment));
        }
        let Some(blob_ref) = attachment.blob_ref.clone() else {
            return Ok(Some(attachment));
        };
        // В облачном режиме ключа файла в сообщении нет: сервер расшифровал
        // файл сам (ключом аккаунта) и отдал открытое содержимое.
        let (key, nonce) = match (attachment.key.clone(), attachment.nonce.clone()) {
            (Some(key), Some(nonce)) => (key, nonce),
            _ => (Vec::new(), Vec::new()),
        };
        let data = self.download_blob(&blob_ref, &key, &nonce).await?;
        // Кэшируем содержимое локально (с лимитом и вытеснением старых) —
        // повторное открытие не качает файл заново.
        let _ = self
            .inner
            .storage
            .cache_attachment_data(server_id, &data, MAX_ATTACHMENT_CACHE_BYTES)
            .await;
        Ok(Some(Attachment { data, ..attachment }))
    }

    /// Содержимое постера видео (если он есть) — скачивается и расшифровывается.
    pub async fn attachment_thumb_content(
        &self,
        server_id: u64,
    ) -> Result<Option<Vec<u8>>, CoreError> {
        let Some(message) = self.message_by_server_id(server_id).await? else {
            return Ok(None);
        };
        let Some(thumb) = message.attachment.as_ref().and_then(|a| a.thumb.as_ref()) else {
            return Ok(None);
        };
        if !thumb.data.is_empty() {
            return Ok(Some(thumb.data.clone()));
        }
        if thumb.reference.is_empty() || thumb.key.is_empty() {
            return Ok(None);
        }
        match self
            .download_blob(&thumb.reference, &thumb.key, &thumb.nonce)
            .await
        {
            Ok(data) => Ok(Some(data)),
            Err(_) => Ok(None),
        }
    }

    /// Загружает файл в хранилище сервера (зашифрованным) и возвращает
    /// (ссылка на блоб, ключ, nonce).
    /// Загружает файл на сервер.
    ///
    /// `cloud` — облачный режим: файл шифруется ключом аккаунта, который
    /// знает сервер (как DC в облачных чатах Telegram), и сервер может
    /// дедуплицировать его по отпечатку содержимого.  В приватном режиме файл
    /// шифруется случайным ключом, который едет в сообщении, — сервер видит
    /// только шифротекст.
    ///
    /// Возвращает (ссылка на блоб, ключ файла, nonce).  В облачном режиме
    /// ключ и nonce пустые: сервер сам отдаёт участникам чата открытое
    /// содержимое файла.
    async fn upload_blob(
        &self,
        chat_id: &str,
        data: &[u8],
        cloud: bool,
        meta: Option<&FileMeta>,
    ) -> Result<(String, Vec<u8>, Vec<u8>), CoreError> {
        let crypto = self
            .inner
            .crypto
            .as_ref()
            .ok_or_else(|| CoreError::E2eUnavailable("no crypto provider".into()))?
            .clone();
        // Дедупликация: тот же файл уже заливали — переиспользуем блоб и ключ
        // (ключ остаётся на устройстве, сервер его не знает).
        let hash = crypto.hash_blob(data).await?;
        if !cloud {
            if let Some(cached) = self.inner.storage.find_uploaded_blob(&hash).await? {
                if cached.size == data.len() as u64 {
                    return Ok((cached.blob_ref, cached.key, cached.nonce));
                }
            }
        }
        // Ключ файла: в облачном режиме — ключ аккаунта (его знает сервер),
        // в приватном — случайный на файл (ездит в сообщении).
        let key = if cloud {
            self.inner.cloud_account_key().await?
        } else {
            let mut key = [0u8; 32];
            getrandom::fill(&mut key).map_err(|e| CoreError::Internal(format!("blob key: {e}")))?;
            key
        };
        let (ciphertext, nonce) = crypto.encrypt_blob(&key, data).await?;
        let blob_id = random_hex(16)?;

        let mut offset = 0usize;
        let mut deduplicated_ref = String::new();
        while offset < ciphertext.len() {
            let end = (offset + BLOB_CHUNK_BYTES).min(ciphertext.len());
            let response = self
                .request(proto::request::Method::UploadBlob(
                    proto::UploadBlobRequest {
                        blob_id: blob_id.clone(),
                        chat_id: chat_id.to_string(),
                        offset: offset as u64,
                        total_size: ciphertext.len() as u32,
                        data: ciphertext[offset..end].to_vec(),
                        cloud,
                        content_hash: hash.to_vec(),
                        nonce: nonce.clone(),
                        name: meta.map(|m| m.name.clone()).unwrap_or_default(),
                        kind: meta.map(|m| m.kind.clone()).unwrap_or_default(),
                        mime: meta.map(|m| m.mime.clone()).unwrap_or_default(),
                        plaintext_size: meta.map(|m| m.plaintext_size).unwrap_or_default(),
                    },
                ))
                .await?;
            match response {
                proto::response::Result::UploadBlob(result) => {
                    // Сервер уже хранит такой файл — докачивать нечего.
                    if result.deduplicated {
                        deduplicated_ref = result.blob_ref;
                        break;
                    }
                }
                _ => {
                    let detail = match &response {
                        proto::response::Result::Error(error) => {
                            format!("error {}: {}", error.code, error.message)
                        }
                        other => format!("{:?}", std::mem::discriminant(other)),
                    };
                    return Err(CoreError::Protocol(format!(
                        "unexpected upload_blob response: {detail}"
                    )));
                }
            }
            offset = end;
        }
        if !deduplicated_ref.is_empty() {
            // Файл уже на сервере: ключ файла не нужен (сервер отдаёт открытое
            // содержимое), сохраняем только ссылку.
            return Ok((deduplicated_ref, Vec::new(), Vec::new()));
        }

        let response = self
            .request(proto::request::Method::FinishBlob(
                proto::FinishBlobRequest {
                    blob_id: blob_id.clone(),
                    chat_id: chat_id.to_string(),
                    total_size: ciphertext.len() as u32,
                },
            ))
            .await?;
        match response {
            proto::response::Result::FinishBlob(result) => {
                if !cloud {
                    // Запоминаем загрузку: повторная отправка того же файла
                    // не потребует новой заливки на сервер.
                    let _ = self
                        .inner
                        .storage
                        .save_uploaded_blob(
                            &hash,
                            &result.blob_ref,
                            &key,
                            &nonce,
                            data.len() as u64,
                        )
                        .await;
                    Ok((result.blob_ref, key.to_vec(), nonce))
                } else {
                    // Облачный режим: дедупликация сервера (по отпечатку
                    // содержимого), локальный кэш не нужен — ключ файла
                    // сервер знает и сам.
                    Ok((result.blob_ref, Vec::new(), Vec::new()))
                }
            }
            _ => Err(CoreError::Protocol(
                "unexpected finish_blob response".into(),
            )),
        }
    }

    /// Скачивает блоб с сервера и расшифровывает его ключом из сообщения.
    /// Скачивает файл.  В приватном режиме сервер отдаёт шифротекст, и мы
    /// расшифровываем его ключом из сообщения; в облачном режиме сервер сам
    /// расшифровывает файл ключом аккаунта и отдаёт открытое содержимое
    /// (поэтому `key`/`nonce` пустые).
    async fn download_blob(
        &self,
        blob_ref: &str,
        key: &[u8],
        nonce: &[u8],
    ) -> Result<Vec<u8>, CoreError> {
        let crypto = self
            .inner
            .crypto
            .as_ref()
            .ok_or_else(|| CoreError::E2eUnavailable("no crypto provider".into()))?
            .clone();
        let mut ciphertext: Vec<u8> = Vec::new();
        let mut offset = 0u64;
        loop {
            let response = self
                .request(proto::request::Method::DownloadBlob(
                    proto::DownloadBlobRequest {
                        blob_ref: blob_ref.to_string(),
                        offset,
                        length: BLOB_CHUNK_BYTES as u32,
                    },
                ))
                .await?;
            let proto::response::Result::DownloadBlob(result) = response else {
                let detail = match &response {
                    proto::response::Result::Error(error) => {
                        format!("error {}: {}", error.code, error.message)
                    }
                    other => format!("{:?}", std::mem::discriminant(other)),
                };
                return Err(CoreError::Protocol(format!(
                    "unexpected download_blob response: {detail}"
                )));
            };
            let received = result.data.len() as u64;
            ciphertext.extend_from_slice(&result.data);
            offset += received;
            if result.eof || received == 0 {
                break;
            }
        }
        if key.is_empty() {
            // Облачный режим: сервер уже расшифровал файл ключом аккаунта.
            return Ok(ciphertext);
        }
        let key: [u8; 32] = key
            .try_into()
            .map_err(|_| CoreError::InvalidInput("invalid blob key".into()))?;
        crypto
            .decrypt_blob(&key, nonce, &ciphertext)
            .await
            .map_err(CoreError::Crypto)
    }

    /// Возвращает упавшее сообщение в очередь отправки.
    ///
    /// Повтор не создаёт новую копию: в очередь возвращается та же запись с
    /// тем же ключом идемпотентности, поэтому сервер не продублирует
    /// сообщение, даже если предыдущая попытка дошла до сервера.
    pub async fn retry_message(&self, local_id: &str) -> Result<bool, CoreError> {
        self.start().await?;
        if local_id.trim().is_empty() {
            return Err(CoreError::InvalidInput("local_id is required".into()));
        }
        let requeued = self.inner.storage.requeue_outbox(local_id).await?;
        if requeued {
            // Разбудить обработчик очереди: сообщение уйдёт в ближайшем тике.
            let _ = self.inner.command_tx.try_send(Command::Kick);
        }
        Ok(requeued)
    }

    /// Общая часть отправки: собирает и шифрует payload, ставит его в outbox.
    async fn send_payload(
        &self,
        chat_id: &str,
        payload: Payload,
        reply_to: Option<u64>,
        grouped_id: Option<&str>,
    ) -> Result<String, CoreError> {
        self.start().await?;
        self.refresh_before_compose().await;
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
        let (request, key_shares) = crate::send::compose_send_request(
            &self.inner,
            &local_id,
            fetch,
            &chat,
            &account,
            &payload,
            &crate::send::SendOptions {
                reply_to,
                grouped_id: grouped_id.map(str::to_string),
            },
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
            content_kind: payload.content_kind(),
            body: Some(payload.text.clone()),
            attachment: payload.attachment.clone(),
            reply_to_server_id: reply_to,
            grouped_id: grouped_id.map(str::to_string),
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
        // Ключ групповой сессии уже в запросе: фиксируем выдачу, чтобы не
        // дублировать ключ этим устройствам (сбой записи не критичен).
        if !key_shares.is_empty() {
            if let Err(error) = self
                .inner
                .storage
                .add_group_key_shares(chat_id, &request.group_session_id, &key_shares)
                .await
            {
                warn!(error = %error, chat_id = %chat_id, "failed to record group key shares");
            }
        }
        self.inner.emit(UiEvent::OutboxChanged {
            local_id: local_id.clone(),
            state: OutboxState::Pending,
            error: None,
            error_code: None,
        });
        // Разбудить обработчик outbox; при переполнении канала тик сделает это сам.
        let _ = self.inner.command_tx.try_send(Command::Kick);
        Ok(local_id)
    }

    /// Редактирует текст собственного сообщения (только автор; права
    /// окончательно проверяет сервер). Для E2E-чата новый текст шифруется для
    /// всех активных устройств участников, поэтому правка требует сети.
    /// Открытый текст E2E-чата на сервер не попадает. Возвращает обновлённую
    /// локальную запись.
    pub async fn edit_text(
        &self,
        chat_id: &str,
        server_id: u64,
        text: &str,
    ) -> Result<MessageRecord, CoreError> {
        if server_id == 0 {
            return Err(CoreError::InvalidInput("server_id is required".into()));
        }
        if text.is_empty() {
            return Err(CoreError::InvalidInput("empty message".into()));
        }
        rchat_proto::validate::chat_id(chat_id)
            .map_err(|e| CoreError::InvalidInput(e.to_string()))?;
        self.start().await?;
        self.refresh_before_compose().await;
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

        // Как и при отправке: обновляем кэш устройств участников, иначе правка
        // может не дойти до нового устройства собеседника.
        if chat.e2e_mode == proto::E2eMode::OlmV1 {
            let members = self.inner.storage.chat_members(chat_id).await?;
            let user_ids: Vec<String> = members.iter().map(|m| m.user_id.clone()).collect();
            let _ = self.refresh_devices(&user_ids).await;
        }

        let client = self.clone();
        let fetch = move |ids: Vec<String>| async move { client.fetch_bundles(&ids).await };
        // Правка меняет только текст: вложение (если оно было) сохраняется.
        let stored = self
            .inner
            .storage
            .get_message_by_server(server_id)
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("message {server_id}")))?;
        let mut payload = Payload::text(text);
        payload.attachment = stored.attachment;
        let (request, key_shares) = crate::send::compose_edit_request(
            &self.inner,
            fetch,
            &chat,
            &account,
            server_id,
            &payload,
        )
        .await?;
        let response = self
            .request(proto::request::Method::EditMessage(request))
            .await?;
        let view = match response {
            proto::response::Result::EditMessage(response) => response
                .message
                .ok_or_else(|| CoreError::Protocol("edit response without message".into()))?,
            other => return Err(unexpected(other)),
        };
        // Правка доставлена (в т.ч. с ключом групповой сессии) — фиксируем
        // выдачу; сбой записи не критичен.
        if !key_shares.is_empty() {
            if let Err(error) = self
                .inner
                .storage
                .add_group_key_shares(chat_id, &view.group_session_id, &key_shares)
                .await
            {
                warn!(error = %error, chat_id = %chat_id, "failed to record group key shares");
            }
        }
        let edited_at_ms = if view.edited_at_ms != 0 {
            view.edited_at_ms
        } else {
            rchat_proto::now_ms()
        };
        // Локально знаем точный текст и вложение: не дожидаемся события
        // синхронизации.
        self.apply_local(&[SyncApply::EditMessage {
            server_id,
            content_kind: view.content_kind,
            body: Some(text.to_string()),
            attachment: payload.attachment,
            edited_at_ms,
        }])
        .await?;
        let record = self.load_message_record(server_id).await?;
        self.inner.emit(UiEvent::MessageUpdated(record.clone()));
        Ok(record)
    }

    /// Удаляет собственное сообщение для всех участников (tombstone; политика
    /// MVP — без «удалить только для себя»). Сервер стирает ciphertext-ы и
    /// dev-plaintext, остальные устройства получают событие из журнала.
    /// Возвращает обновлённую локальную запись.
    pub async fn delete_message(
        &self,
        chat_id: &str,
        server_id: u64,
    ) -> Result<MessageRecord, CoreError> {
        if server_id == 0 {
            return Err(CoreError::InvalidInput("server_id is required".into()));
        }
        rchat_proto::validate::chat_id(chat_id)
            .map_err(|e| CoreError::InvalidInput(e.to_string()))?;
        self.start().await?;
        // Если запись известна локально, она обязана принадлежать этому чату.
        if let Some(message) = self.inner.storage.get_message_by_server(server_id).await? {
            if message.chat_id != chat_id {
                return Err(CoreError::InvalidInput(format!(
                    "message {server_id} belongs to another chat"
                )));
            }
        }
        let response = self
            .request(proto::request::Method::DeleteMessage(
                proto::DeleteMessageRequest { server_id },
            ))
            .await?;
        let view = match response {
            proto::response::Result::DeleteMessage(response) => response
                .message
                .ok_or_else(|| CoreError::Protocol("delete response without message".into()))?,
            other => return Err(unexpected(other)),
        };
        let deleted_at_ms = if view.deleted_at_ms != 0 {
            view.deleted_at_ms
        } else {
            rchat_proto::now_ms()
        };
        self.apply_local(&[SyncApply::DeleteMessage {
            server_id,
            deleted_at_ms,
        }])
        .await?;
        let record = self.load_message_record(server_id).await?;
        self.inner.emit(UiEvent::MessageUpdated(record.clone()));
        Ok(record)
    }

    /// Сохраняет мутацию от имени собственной операции, не трогая курсор
    /// синхронизации (событие с сервера придёт позже и идемпотентно повторит её).
    async fn apply_local(&self, ops: &[SyncApply]) -> Result<(), CoreError> {
        let cursor = self.inner.storage.get_sync_cursor().await?;
        self.inner.storage.apply_sync_batch(ops, cursor).await?;
        Ok(())
    }

    /// Локальная запись сообщения по `server_id` (с вложением, если оно есть).
    pub async fn message_by_server_id(
        &self,
        server_id: u64,
    ) -> Result<Option<MessageRecord>, CoreError> {
        Ok(self.inner.storage.get_message_by_server(server_id).await?)
    }

    async fn load_message_record(&self, server_id: u64) -> Result<MessageRecord, CoreError> {
        self.inner
            .storage
            .get_message_by_server(server_id)
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("message {server_id}")))
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

    /// Последние известные подтверждения доставки/прочтения по чату
    /// (локальный кеш, наполняется событиями журнала и своими действиями).
    pub async fn receipts(&self, chat_id: &str) -> Result<Vec<ReceiptRecord>, CoreError> {
        Ok(self.inner.storage.get_receipts(chat_id).await?)
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

fn validate_call_id(call_id: &str) -> Result<(), CoreError> {
    rchat_proto::validate::call_id(call_id)
        .map_err(|error| CoreError::InvalidInput(error.to_string()))
}

fn call_record(call: Option<proto::Call>) -> Result<CallRecord, CoreError> {
    call.map(|call| convert::call_from_proto(&call))
        .ok_or_else(|| CoreError::Protocol("call response without call".into()))
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

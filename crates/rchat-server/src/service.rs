//! Прикладная логика сервера: обработка методов протокола.
//!
//! Каждый обработчик проверяет аутентификацию, авторизацию (принадлежность
//! к чату), валидацию входа и лимиты — до записи в хранилище. Изменения,
//! требующие атомарности (регистрация, вставка сообщения с событиями,
//! выдача предключа), выполняются репозиторием в одной транзакции.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rchat_proto::proto;
use rchat_proto::proto::{request, response};
use sha2::{Digest, Sha256};

use crate::calls::{ActiveCall, CallError, CallRegistry};
use crate::config::ServerConfig;
use crate::delivery::Connections;
use crate::error::ServerError;
use crate::model::InsertMessageOutcome;
use crate::model::{
    ChatRecord, DeviceRecord, MemberRecord, MessageRecord, MessageUpdate, NewDevice, NewMessage,
    NewSession, NewSyncEvent, NewUser, PreKeyRecord, ReceiptRecord, RecipientRecord, SessionRecord,
    UserRecord,
};
use crate::password::PasswordHasher;
use crate::proto_util as p;
use crate::rate::RateLimiter;
use crate::repo::Repo;

/// Идентификатор пира (для логов и лимитов).
#[derive(Debug, Clone)]
pub struct PeerInfo {
    /// Адрес соединения (для логов).
    pub addr: String,
    /// Ключ для лимитов (обычно IP).
    pub rate_key: String,
}

/// Состояние соединения: аутентификация и отметки событий жизненного цикла.
pub struct ConnState {
    pub peer: PeerInfo,
    pub auth: Option<AuthContext>,
    pub just_authenticated: bool,
    pub just_logged_out: bool,
}

/// Контекст аутентифицированного запроса.
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub user: UserRecord,
    pub device: DeviceRecord,
    pub session: SessionRecord,
    pub last_touch: Instant,
}

impl ConnState {
    pub fn new(peer: PeerInfo) -> Self {
        Self {
            peer,
            auth: None,
            just_authenticated: false,
            just_logged_out: false,
        }
    }
}

/// Сервер rchat: обработчики + доступ к хранилищу и реестру соединений.
pub struct Server {
    repo: Arc<dyn Repo>,
    config: ServerConfig,
    hasher: Arc<dyn PasswordHasher>,
    connections: Arc<Connections>,
    limiter: Arc<RateLimiter>,
    /// Активные звонки (только сигнализация; in-memory, не журналируются).
    calls: CallRegistry,
    /// Монотонный счётчик server_id (единственный экземпляр сервера).
    next_server_id: AtomicU64,
}

impl Server {
    /// Создаёт сервер и продолжает нумерацию сообщений с максимального id в БД.
    pub async fn start(
        repo: Arc<dyn Repo>,
        config: ServerConfig,
        hasher: Arc<dyn PasswordHasher>,
    ) -> Result<Self, ServerError> {
        let max_id = repo.max_message_server_id().await?;
        Ok(Self {
            repo,
            limiter: Arc::new(RateLimiter::new(config.rate_limits)),
            config,
            hasher,
            connections: Arc::new(Connections::new()),
            calls: CallRegistry::new(),
            next_server_id: AtomicU64::new(max_id),
        })
    }

    /// Конструктор для тестов с уже известным состоянием хранилища.
    #[doc(hidden)]
    pub fn with_state(
        repo: Arc<dyn Repo>,
        config: ServerConfig,
        hasher: Arc<dyn PasswordHasher>,
        connections: Arc<Connections>,
        next_server_id: u64,
    ) -> Self {
        Self {
            repo,
            limiter: Arc::new(RateLimiter::new(config.rate_limits)),
            config,
            hasher,
            connections,
            calls: CallRegistry::new(),
            next_server_id: AtomicU64::new(next_server_id),
        }
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    pub fn repo(&self) -> &Arc<dyn Repo> {
        &self.repo
    }

    pub fn connections(&self) -> &Arc<Connections> {
        &self.connections
    }

    fn next_message_id(&self) -> u64 {
        self.next_server_id.fetch_add(1, Ordering::SeqCst) + 1
    }

    // -----------------------------------------------------------------
    // Верхний уровень: диспетчер и преобразование ошибок
    // -----------------------------------------------------------------

    /// Обрабатывает запрос; ошибки превращаются в `response::Result::Error`.
    pub async fn handle_request(
        &self,
        state: &mut ConnState,
        method: request::Method,
    ) -> response::Result {
        match self.dispatch(state, method).await {
            Ok(result) => result,
            Err(error) => response::Result::Error(rchat_proto::api_error(
                error.code,
                error.message.clone(),
                error.retryable,
            )),
        }
    }

    async fn dispatch(
        &self,
        state: &mut ConnState,
        method: request::Method,
    ) -> Result<response::Result, ServerError> {
        use request::Method as M;
        // Сессия может истечь, пока соединение живо: права снимаются сразу,
        // следующий запрос получает ту же ошибку, что и при Resume.
        let session_expired = state
            .auth
            .as_ref()
            .is_some_and(|auth| auth.session.expires_at_ms <= rchat_proto::now_ms());
        if session_expired {
            state.auth = None;
            return Err(ServerError::session_expired());
        }
        let result = match method {
            M::Register(request) => self.handle_register(state, request).await?,
            M::Login(request) => self.handle_login(state, request).await?,
            M::Resume(request) => self.handle_resume(state, request).await?,
            M::Logout(_) => self.handle_logout(state).await?,
            M::GetMe(_) => {
                let auth = self.authed(state)?;
                response::Result::GetMe(proto::GetMeResponse {
                    user: Some(p::user_to_proto(&auth.user)),
                    device: Some(p::device_to_proto(&auth.device)),
                })
            }
            M::UploadKeys(request) => {
                let auth = self.authed(state)?;
                self.handle_upload_keys(&auth, request).await?
            }
            M::GetDeviceBundles(request) => {
                let auth = self.authed(state)?;
                self.handle_get_device_bundles(&auth, request).await?
            }
            M::GetDevices(request) => {
                let auth = self.authed(state)?;
                self.handle_get_devices(&auth, request).await?
            }
            M::ListDevices(_) => {
                let auth = self.authed(state)?;
                self.handle_list_devices(&auth).await?
            }
            M::CreateDirectChat(request) => {
                let auth = self.authed(state)?;
                self.handle_create_direct_chat(&auth, request).await?
            }
            M::ListChats(_) => {
                let auth = self.authed(state)?;
                self.handle_list_chats(&auth).await?
            }
            M::SendMessage(request) => {
                let auth = self.authed(state)?;
                self.handle_send_message(&auth, request).await?
            }
            M::GetHistory(request) => {
                let auth = self.authed(state)?;
                self.handle_get_history(&auth, request).await?
            }
            M::EditMessage(request) => {
                let auth = self.authed(state)?;
                self.handle_edit_message(&auth, request).await?
            }
            M::DeleteMessage(request) => {
                let auth = self.authed(state)?;
                self.handle_delete_message(&auth, request).await?
            }
            M::MarkReceipt(request) => {
                let auth = self.authed(state)?;
                self.handle_mark_receipt(&auth, request).await?
            }
            M::CreateGroupChat(request) => {
                let auth = self.authed(state)?;
                self.handle_create_group_chat(&auth, request).await?
            }
            M::InviteToChat(request) => {
                let auth = self.authed(state)?;
                self.handle_invite_to_chat(&auth, request).await?
            }
            M::LeaveChat(request) => {
                let auth = self.authed(state)?;
                self.handle_leave_chat(&auth, request).await?
            }
            M::RemoveChatMember(request) => {
                let auth = self.authed(state)?;
                self.handle_remove_chat_member(&auth, request).await?
            }
            M::RenameChat(request) => {
                let auth = self.authed(state)?;
                self.handle_rename_chat(&auth, request).await?
            }
            M::CallInvite(request) => {
                let auth = self.authed(state)?;
                self.handle_call_invite(&auth, request).await?
            }
            M::CallAccept(request) => {
                let auth = self.authed(state)?;
                self.handle_call_accept(&auth, request).await?
            }
            M::CallDecline(request) => {
                let auth = self.authed(state)?;
                self.handle_call_decline(&auth, request).await?
            }
            M::UploadBlob(request) => {
                let auth = self.authed(state)?;
                self.handle_upload_blob(&auth, request).await?
            }
            M::FinishBlob(request) => {
                let auth = self.authed(state)?;
                self.handle_finish_blob(&auth, request).await?
            }
            M::DownloadBlob(request) => {
                let auth = self.authed(state)?;
                self.handle_download_blob(&auth, request).await?
            }
            M::AbortBlob(request) => {
                let auth = self.authed(state)?;
                self.handle_abort_blob(&auth, request).await?
            }
            M::CallEnd(request) => {
                let auth = self.authed(state)?;
                self.handle_call_end(&auth, request).await?
            }
            M::Sync(request) => {
                let auth = self.authed(state)?;
                self.handle_sync(&auth, request).await?
            }
            M::GetSnapshot(_) => {
                let auth = self.authed(state)?;
                self.handle_get_snapshot(&auth).await?
            }
            M::RevokeDevice(request) => {
                self.authed(state)?;
                self.handle_revoke_device(state, request).await?
            }
        };
        // Периодическое обновление «последней активности» сессии и устройства.
        if let Some(auth) = state.auth.as_mut() {
            if auth.last_touch.elapsed() >= self.config.session_touch_interval() {
                auth.last_touch = Instant::now();
                let now = rchat_proto::now_ms();
                if let Err(error) = self.repo.touch_session(&auth.session.session_id, now).await {
                    tracing::warn!(error = %error, "failed to touch session");
                }
                let (user_id, device_id) =
                    (auth.user.user_id.clone(), auth.device.device_id.clone());
                if let Err(error) = self.repo.touch_device(&user_id, &device_id, now).await {
                    tracing::warn!(error = %error, "failed to touch device");
                }
            }
        }
        Ok(result)
    }

    fn authed(&self, state: &ConnState) -> Result<AuthContext, ServerError> {
        state.auth.clone().ok_or_else(ServerError::unauthenticated)
    }

    fn require_anonymous(&self, state: &ConnState) -> Result<(), ServerError> {
        if state.auth.is_some() {
            return Err(ServerError::invalid_request("already authenticated"));
        }
        Ok(())
    }

    // -----------------------------------------------------------------
    // Регистрация, вход, сессия
    // -----------------------------------------------------------------

    async fn handle_register(
        &self,
        state: &mut ConnState,
        request: proto::RegisterRequest,
    ) -> Result<response::Result, ServerError> {
        self.require_anonymous(state)?;
        if !self.limiter.register_allowed(&state.peer.rate_key) {
            return Err(ServerError::rate_limited());
        }
        let username = validate_username(&request.username)?;
        let display_name = validate_display_name(&request.display_name)?;
        validate_password(&request.password)?;
        let device_proto = request
            .device
            .as_ref()
            .ok_or_else(|| ServerError::invalid_request("device is required"))?;
        let device = new_device_from_proto(
            device_proto,
            &request.one_time_keys,
            request.fallback_key.as_ref(),
        )?;

        if self.repo.find_user_by_username(&username).await?.is_some() {
            return Err(ServerError::username_taken());
        }

        let now = rchat_proto::now_ms();
        let password_hash = self.hash_password(&request.password).await?;
        let (token, session) = self.new_session(&device.device_id, now);
        let user = NewUser {
            username,
            display_name,
            password_hash,
            created_at_ms: now,
        };
        let (user, session) = self
            .repo
            .create_user_with_device(&user, &device, &session)
            .await?;
        let device_row = self
            .repo
            .find_device(&user.user_id, &device.device_id)
            .await?
            .ok_or_else(|| ServerError::internal("device not found after registration"))?;

        state.auth = Some(AuthContext {
            user: user.clone(),
            device: device_row.clone(),
            session: session.clone(),
            last_touch: Instant::now(),
        });
        state.just_authenticated = true;

        Ok(response::Result::Register(proto::RegisterResponse {
            user_id: user.user_id,
            session_token: token,
            session_expires_at_ms: session.expires_at_ms,
            device: Some(p::device_to_proto(&device_row)),
        }))
    }

    async fn handle_login(
        &self,
        state: &mut ConnState,
        request: proto::LoginRequest,
    ) -> Result<response::Result, ServerError> {
        self.require_anonymous(state)?;
        let username = validate_username(&request.username)?;
        if !self.limiter.login_allowed(&username) {
            return Err(ServerError::rate_limited());
        }
        let user = self
            .repo
            .find_user_by_username(&username)
            .await?
            .ok_or_else(ServerError::invalid_credentials)?;
        if user.disabled {
            return Err(ServerError::forbidden("account is disabled"));
        }
        if !self
            .verify_password(&request.password, &user.password_hash)
            .await?
        {
            return Err(ServerError::invalid_credentials());
        }
        let device_proto = request
            .device
            .as_ref()
            .ok_or_else(|| ServerError::invalid_request("device is required"))?;
        let new_device = new_device_from_proto(
            device_proto,
            &request.one_time_keys,
            request.fallback_key.as_ref(),
        )?;

        // Устройство: создаём или сверяем ключи.
        let mut device_added = false;
        let existing = self
            .repo
            .find_device(&user.user_id, &new_device.device_id)
            .await?;
        let device = match existing {
            Some(existing) => {
                if existing.is_revoked() {
                    return Err(ServerError::device_conflict("device has been revoked"));
                }
                if new_device.e2e_version != rchat_proto::E2E_VERSION_NONE {
                    if existing.has_e2e_keys() {
                        if existing.ed25519 != new_device.ed25519
                            || existing.curve25519 != new_device.curve25519
                        {
                            return Err(ServerError::device_conflict(
                                "device keys do not match registered keys",
                            ));
                        }
                    } else {
                        // Обновление устройства «без E2E» до устройства с ключами.
                        self.repo
                            .update_device_keys(
                                &user.user_id,
                                &existing.device_id,
                                new_device.e2e_version,
                                &new_device.ed25519,
                                &new_device.curve25519,
                            )
                            .await?;
                    }
                }
                if !new_device.one_time_keys.is_empty() || new_device.fallback_key.is_some() {
                    self.repo
                        .store_prekeys(
                            &user.user_id,
                            &new_device.device_id,
                            &new_device.one_time_keys,
                            new_device.fallback_key.as_ref(),
                        )
                        .await?;
                }
                self.repo
                    .find_device(&user.user_id, &new_device.device_id)
                    .await?
                    .ok_or_else(|| ServerError::internal("device disappeared during login"))?
            }
            None => {
                device_added = true;
                self.repo.create_device(&user.user_id, &new_device).await?
            }
        };

        let now = rchat_proto::now_ms();
        self.repo
            .touch_device(&user.user_id, &device.device_id, now)
            .await?;
        let (token, session) = self.new_session(&device.device_id, now);
        let session = self.repo.create_session(&user.user_id, &session).await?;

        if device_added {
            self.fanout_device_change(
                &user.user_id,
                &device.device_id,
                proto::DeviceChangeKind::Added,
            )
            .await;
        }

        let devices = self.repo.list_devices(&user.user_id, false).await?;
        state.auth = Some(AuthContext {
            user,
            device: device.clone(),
            session: session.clone(),
            last_touch: Instant::now(),
        });
        state.just_authenticated = true;

        Ok(response::Result::Login(proto::LoginResponse {
            user_id: device.user_id.clone(),
            session_token: token,
            session_expires_at_ms: session.expires_at_ms,
            device: Some(p::device_to_proto(&device)),
            devices: devices.iter().map(p::device_to_proto).collect(),
        }))
    }

    async fn handle_resume(
        &self,
        state: &mut ConnState,
        request: proto::ResumeRequest,
    ) -> Result<response::Result, ServerError> {
        self.require_anonymous(state)?;
        let token_hash = sha256(request.session_token.as_bytes());
        let session = self
            .repo
            .find_session_by_token_hash(&token_hash)
            .await?
            .ok_or_else(ServerError::unauthenticated)?;
        let now = rchat_proto::now_ms();
        if session.revoked_at_ms != 0 {
            return Err(ServerError::session_revoked());
        }
        if session.expires_at_ms <= now {
            return Err(ServerError::session_expired());
        }
        let user = self
            .repo
            .find_user_by_id(&session.user_id)
            .await?
            .ok_or_else(ServerError::unauthenticated)?;
        if user.disabled {
            return Err(ServerError::forbidden("account is disabled"));
        }
        let device = self
            .repo
            .find_device(&session.user_id, &session.device_id)
            .await?
            .ok_or_else(ServerError::unauthenticated)?;
        if device.is_revoked() {
            return Err(ServerError::session_revoked());
        }
        self.repo.touch_session(&session.session_id, now).await?;
        self.repo
            .touch_device(&user.user_id, &device.device_id, now)
            .await?;

        state.auth = Some(AuthContext {
            user: user.clone(),
            device: device.clone(),
            session: session.clone(),
            last_touch: Instant::now(),
        });
        state.just_authenticated = true;

        Ok(response::Result::Resume(proto::ResumeResponse {
            user: Some(p::user_to_proto(&user)),
            device: Some(p::device_to_proto(&device)),
            session_expires_at_ms: session.expires_at_ms,
        }))
    }

    async fn handle_logout(&self, state: &mut ConnState) -> Result<response::Result, ServerError> {
        let auth = self.authed(state)?;
        let now = rchat_proto::now_ms();
        self.repo
            .revoke_session(&auth.session.session_id, now)
            .await?;
        state.auth = None;
        state.just_logged_out = true;
        Ok(response::Result::Logout(proto::LogoutResponse {}))
    }

    // -----------------------------------------------------------------
    // Ключи и устройства
    // -----------------------------------------------------------------

    async fn handle_upload_keys(
        &self,
        auth: &AuthContext,
        request: proto::UploadKeysRequest,
    ) -> Result<response::Result, ServerError> {
        if auth.device.e2e_version == rchat_proto::E2E_VERSION_NONE {
            return Err(ServerError::e2e_required(
                "device does not support E2E keys",
            ));
        }
        if request.e2e_version != 0 && request.e2e_version != auth.device.e2e_version {
            return Err(ServerError::invalid_request("e2e version mismatch"));
        }
        let keys = validate_one_time_keys(&request.one_time_keys)?;
        let fallback = match request.fallback_key.as_ref() {
            Some(key) => Some(validate_prekey(key)?),
            None => None,
        };
        let stored = self
            .repo
            .store_prekeys(
                &auth.user.user_id,
                &auth.device.device_id,
                &keys,
                fallback.as_ref(),
            )
            .await?;
        Ok(response::Result::UploadKeys(proto::UploadKeysResponse {
            stored_one_time_keys: stored,
        }))
    }

    async fn handle_get_device_bundles(
        &self,
        auth: &AuthContext,
        request: proto::GetDeviceBundlesRequest,
    ) -> Result<response::Result, ServerError> {
        if request.device_ids.len() > rchat_proto::limits::MAX_DEVICE_IDS_PER_REQUEST {
            return Err(ServerError::invalid_request("too many device ids"));
        }
        let mut seen = BTreeSet::new();
        let mut bundles = Vec::new();
        for device_id in &request.device_ids {
            if !seen.insert(device_id.clone()) {
                continue;
            }
            let Some(device) = self.repo.find_device_by_id(device_id).await? else {
                continue;
            };
            if device.is_revoked() {
                continue;
            }
            if !self.device_visible_to(auth, &device).await? {
                // Не раскрываем существование чужих устройств.
                continue;
            }
            let Some(bundle) = self
                .repo
                .take_bundle(&device.user_id, &device.device_id)
                .await?
            else {
                continue;
            };
            bundles.push(proto::DeviceBundle {
                user_id: bundle.device.user_id.clone(),
                device_id: bundle.device.device_id.clone(),
                keys: Some(proto::DeviceKeys {
                    ed25519: bundle.device.ed25519.clone(),
                    curve25519: bundle.device.curve25519.clone(),
                }),
                one_time_key: bundle.one_time_key.as_ref().map(|key| proto::PreKey {
                    key_id: key.key_id,
                    public_key: key.public_key.clone(),
                }),
                fallback: bundle.fallback,
            });
        }
        Ok(response::Result::GetDeviceBundles(
            proto::GetDeviceBundlesResponse { bundles },
        ))
    }

    async fn handle_get_devices(
        &self,
        auth: &AuthContext,
        request: proto::GetDevicesRequest,
    ) -> Result<response::Result, ServerError> {
        if request.user_ids.len() > rchat_proto::limits::MAX_USER_IDS_PER_REQUEST {
            return Err(ServerError::invalid_request("too many user ids"));
        }
        let mut allowed = BTreeSet::new();
        if request.include_own {
            allowed.insert(auth.user.user_id.clone());
        }
        for user_id in &request.user_ids {
            if allowed.contains(user_id) {
                continue;
            }
            if *user_id == auth.user.user_id {
                allowed.insert(user_id.clone());
                continue;
            }
            if self
                .repo
                .has_shared_chat(&auth.user.user_id, user_id)
                .await?
            {
                allowed.insert(user_id.clone());
            }
            // Чужие пользователи без общего чата молча пропускаются.
        }
        let allowed: Vec<String> = allowed.into_iter().collect();
        let devices = self.repo.devices_for_users(&allowed, false).await?;
        Ok(response::Result::GetDevices(proto::GetDevicesResponse {
            devices: devices.iter().map(p::device_to_proto).collect(),
        }))
    }

    async fn handle_list_devices(
        &self,
        auth: &AuthContext,
    ) -> Result<response::Result, ServerError> {
        let devices = self.repo.list_devices(&auth.user.user_id, true).await?;
        Ok(response::Result::ListDevices(proto::ListDevicesResponse {
            devices: devices.iter().map(p::device_to_proto).collect(),
        }))
    }

    async fn handle_revoke_device(
        &self,
        state: &mut ConnState,
        request: proto::RevokeDeviceRequest,
    ) -> Result<response::Result, ServerError> {
        let auth = self.authed(state)?;
        if request.device_id.is_empty() {
            return Err(ServerError::invalid_request("device id is required"));
        }
        let device = self
            .repo
            .find_device(&auth.user.user_id, &request.device_id)
            .await?
            .ok_or_else(|| ServerError::not_found("device not found"))?;
        if !device.is_revoked() {
            let now = rchat_proto::now_ms();
            self.repo
                .revoke_device(&auth.user.user_id, &device.device_id, now)
                .await?;
            self.fanout_device_change(
                &auth.user.user_id,
                &device.device_id,
                proto::DeviceChangeKind::Revoked,
            )
            .await;
            self.connections
                .disconnect_device(&auth.user.user_id, &device.device_id);
        }
        if state
            .auth
            .as_ref()
            .is_some_and(|a| a.device.device_id == device.device_id)
        {
            // Отозвали свою текущую сессию — соединение остаётся, но без прав.
            state.auth = None;
        }
        Ok(response::Result::RevokeDevice(
            proto::RevokeDeviceResponse {},
        ))
    }

    // -----------------------------------------------------------------
    // Чаты
    // -----------------------------------------------------------------

    async fn handle_create_direct_chat(
        &self,
        auth: &AuthContext,
        request: proto::CreateDirectChatRequest,
    ) -> Result<response::Result, ServerError> {
        let peer_username = validate_username(&request.peer_username)?;
        let mode = proto::E2eMode::try_from(request.e2e_mode)
            .map_err(|_| ServerError::invalid_request("unknown e2e mode"))?;
        if mode == proto::E2eMode::Unspecified {
            return Err(ServerError::invalid_request(
                "e2e mode must be specified explicitly",
            ));
        }
        if mode == proto::E2eMode::NoneDev && !self.config.allow_dev_plaintext_chats {
            return Err(ServerError::forbidden(
                "plaintext dev chats are disabled on this server",
            ));
        }
        let peer = self
            .repo
            .find_user_by_username(&peer_username)
            .await?
            .ok_or_else(|| ServerError::not_found("user not found"))?;
        if peer.user_id == auth.user.user_id {
            return Err(ServerError::invalid_request(
                "cannot create a direct chat with yourself",
            ));
        }
        if mode == proto::E2eMode::OlmV1 {
            self.require_e2e_for_users(&[auth.user.user_id.clone(), peer.user_id.clone()])
                .await?;
        }

        if let Some((chat, members)) = self
            .repo
            .find_direct_chat(&auth.user.user_id, &peer.user_id)
            .await?
        {
            if chat.e2e_mode != mode as i32 {
                return Err(ServerError::conflict(
                    "direct chat already exists with a different e2e mode",
                ));
            }
            let users = vec![auth.user.clone(), peer];
            let chat_proto = p::chat_to_proto(&chat, &members, &users);
            return Ok(response::Result::CreateDirectChat(
                proto::CreateDirectChatResponse {
                    chat: Some(chat_proto),
                },
            ));
        }

        let now = rchat_proto::now_ms();
        let chat = ChatRecord {
            chat_id: random_id()?,
            kind: proto::ChatKind::Direct as i32,
            e2e_mode: mode as i32,
            name: String::new(),
            created_at_ms: now,
            updated_at_ms: now,
            last_server_id: 0,
        };
        let members = vec![
            MemberRecord {
                chat_id: chat.chat_id.clone(),
                user_id: auth.user.user_id.clone(),
                joined_at_ms: now,
                role: 0,
            },
            MemberRecord {
                chat_id: chat.chat_id.clone(),
                user_id: peer.user_id.clone(),
                joined_at_ms: now,
                role: 0,
            },
        ];
        let chat_proto = p::chat_to_proto(&chat, &members, &[auth.user.clone(), peer.clone()]);
        let payload = p::encode_event_payload(p::chat_created_event(chat_proto.clone()))?;
        let target_devices = self
            .repo
            .devices_for_users(&[auth.user.user_id.clone(), peer.user_id.clone()], false)
            .await?;
        let events: Vec<NewSyncEvent> = target_devices
            .iter()
            .map(|device| NewSyncEvent {
                device_id: device.device_id.clone(),
                kind_hint: 10,
                payload: payload.clone(),
                created_at_ms: now,
            })
            .collect();
        let seqs = self.repo.create_chat(&chat, &members, &events).await?;
        for (index, device) in target_devices.iter().enumerate() {
            if let Some((_, seq)) = seqs.get(index) {
                self.push_event(&device.user_id, &device.device_id, &payload, *seq);
            }
        }

        Ok(response::Result::CreateDirectChat(
            proto::CreateDirectChatResponse {
                chat: Some(chat_proto),
            },
        ))
    }

    async fn handle_list_chats(&self, auth: &AuthContext) -> Result<response::Result, ServerError> {
        let chats = self.repo.list_chats_for_user(&auth.user.user_id).await?;
        let mut all_users: BTreeMap<String, UserRecord> = BTreeMap::new();
        for (_, members) in &chats {
            for member in members {
                if all_users.contains_key(&member.user_id) {
                    continue;
                }
                if let Some(user) = self.repo.find_user_by_id(&member.user_id).await? {
                    all_users.insert(member.user_id.clone(), user);
                }
            }
        }
        let users: Vec<UserRecord> = all_users.into_values().collect();
        let chats_proto = chats
            .iter()
            .map(|(chat, members)| p::chat_to_proto(chat, members, &users))
            .collect();
        Ok(response::Result::ListChats(proto::ListChatsResponse {
            chats: chats_proto,
        }))
    }

    async fn members_and_devices(
        &self,
        chat_id: &str,
    ) -> Result<(ChatRecord, Vec<MemberRecord>, Vec<DeviceRecord>), ServerError> {
        let (chat, members) = self
            .repo
            .get_chat(chat_id)
            .await?
            .ok_or_else(|| ServerError::not_found("chat not found"))?;
        let user_ids: Vec<String> = members.iter().map(|m| m.user_id.clone()).collect();
        let devices = self.repo.devices_for_users(&user_ids, false).await?;
        Ok((chat, members, devices))
    }

    // -----------------------------------------------------------------
    // Групповые чаты
    // -----------------------------------------------------------------

    /// Проверяет, что у каждого перечисленного пользователя есть хотя бы одно
    /// неотозванное устройство с E2E-ключами.
    async fn require_e2e_for_users(&self, user_ids: &[String]) -> Result<(), ServerError> {
        let devices = self.repo.devices_for_users(user_ids, false).await?;
        for user_id in user_ids {
            if !devices
                .iter()
                .any(|device| &device.user_id == user_id && device.has_e2e_keys())
            {
                return Err(ServerError::e2e_keys_missing(
                    "every member needs at least one E2E device for an e2e chat",
                ));
            }
        }
        Ok(())
    }

    /// Пользователи-участники для `chat_to_proto`: известные + догруженные.
    async fn member_users(
        &self,
        members: &[MemberRecord],
        known: &[UserRecord],
    ) -> Result<Vec<UserRecord>, ServerError> {
        let mut map: BTreeMap<String, UserRecord> = known
            .iter()
            .map(|user| (user.user_id.clone(), user.clone()))
            .collect();
        let missing: Vec<String> = members
            .iter()
            .map(|member| member.user_id.clone())
            .filter(|id| !map.contains_key(id))
            .collect();
        if !missing.is_empty() {
            for user in self.repo.find_users_by_ids(&missing).await? {
                map.insert(user.user_id.clone(), user);
            }
        }
        Ok(map.into_values().collect())
    }

    /// Готовит журнальные события ChatUpdated для устройств `target_users`.
    async fn chat_updated_fanout(
        &self,
        chat_proto: &proto::Chat,
        target_users: &[String],
        now_ms: u64,
    ) -> Result<(Vec<NewSyncEvent>, Vec<DeviceRecord>, Vec<u8>), ServerError> {
        let payload = p::encode_event_payload(p::chat_updated_event(chat_proto.clone()))?;
        let devices = self.repo.devices_for_users(target_users, false).await?;
        let events = devices
            .iter()
            .map(|device| NewSyncEvent {
                device_id: device.device_id.clone(),
                kind_hint: 16,
                payload: payload.clone(),
                created_at_ms: now_ms,
            })
            .collect();
        Ok((events, devices, payload))
    }

    /// Пишет push-событие всем устройствам, которым repo назначил seq.
    fn push_chat_events(&self, devices: &[DeviceRecord], payload: &[u8], seqs: &[(String, u64)]) {
        for (index, device) in devices.iter().enumerate() {
            if let Some((_, seq)) = seqs.get(index) {
                self.push_event(&device.user_id, &device.device_id, payload, *seq);
            }
        }
    }

    /// Приводит снимок чата к состоянию «обновлён сейчас».
    fn chat_after_update(chat: &ChatRecord, now_ms: u64) -> ChatRecord {
        ChatRecord {
            updated_at_ms: now_ms.max(chat.updated_at_ms),
            ..chat.clone()
        }
    }

    fn group_max_members(&self) -> usize {
        self.config
            .max_group_members
            .min(rchat_proto::limits::MAX_GROUP_MEMBERS_HARD)
    }

    async fn handle_create_group_chat(
        &self,
        auth: &AuthContext,
        request: proto::CreateGroupChatRequest,
    ) -> Result<response::Result, ServerError> {
        let name = validate_group_name(&request.name)?;
        let mode = proto::E2eMode::try_from(request.e2e_mode)
            .map_err(|_| ServerError::invalid_request("unknown e2e mode"))?;
        if mode == proto::E2eMode::Unspecified {
            return Err(ServerError::invalid_request(
                "e2e mode must be specified explicitly",
            ));
        }
        if mode == proto::E2eMode::NoneDev && !self.config.allow_dev_plaintext_chats {
            return Err(ServerError::forbidden(
                "plaintext dev chats are disabled on this server",
            ));
        }
        if request.member_usernames.len() > rchat_proto::limits::MAX_INVITES_PER_REQUEST {
            return Err(ServerError::limit_exceeded(
                "too many members in one request",
            ));
        }
        let max_members = self.group_max_members();

        let mut invited: Vec<UserRecord> = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for raw in &request.member_usernames {
            let username = validate_username(raw)?;
            let user = self
                .repo
                .find_user_by_username(&username)
                .await?
                .ok_or_else(|| ServerError::not_found("user not found"))?;
            if user.user_id == auth.user.user_id || !seen.insert(user.user_id.clone()) {
                continue;
            }
            invited.push(user);
        }
        if 1 + invited.len() > max_members {
            return Err(ServerError::limit_exceeded(
                "group chat member limit reached",
            ));
        }

        let now = rchat_proto::now_ms();
        let chat_id = random_id()?;
        let mut members: Vec<MemberRecord> = Vec::with_capacity(invited.len() + 1);
        members.push(MemberRecord {
            chat_id: chat_id.clone(),
            user_id: auth.user.user_id.clone(),
            joined_at_ms: now,
            role: proto::ChatMemberRole::Owner as i32,
        });
        for user in &invited {
            members.push(MemberRecord {
                chat_id: chat_id.clone(),
                user_id: user.user_id.clone(),
                joined_at_ms: now,
                role: proto::ChatMemberRole::Member as i32,
            });
        }
        if mode == proto::E2eMode::OlmV1 {
            let user_ids: Vec<String> = members.iter().map(|m| m.user_id.clone()).collect();
            self.require_e2e_for_users(&user_ids).await?;
        }

        let chat = ChatRecord {
            chat_id,
            kind: proto::ChatKind::Group as i32,
            e2e_mode: mode as i32,
            name,
            created_at_ms: now,
            updated_at_ms: now,
            last_server_id: 0,
        };
        let users = self
            .member_users(&members, std::slice::from_ref(&auth.user))
            .await?;
        let chat_proto = p::chat_to_proto(&chat, &members, &users);
        let payload = p::encode_event_payload(p::chat_created_event(chat_proto.clone()))?;
        let user_ids: Vec<String> = members.iter().map(|m| m.user_id.clone()).collect();
        let devices = self.repo.devices_for_users(&user_ids, false).await?;
        let events: Vec<NewSyncEvent> = devices
            .iter()
            .map(|device| NewSyncEvent {
                device_id: device.device_id.clone(),
                kind_hint: 10,
                payload: payload.clone(),
                created_at_ms: now,
            })
            .collect();
        let seqs = self.repo.create_chat(&chat, &members, &events).await?;
        self.push_chat_events(&devices, &payload, &seqs);

        Ok(response::Result::CreateGroupChat(
            proto::CreateGroupChatResponse {
                chat: Some(chat_proto),
            },
        ))
    }

    async fn handle_invite_to_chat(
        &self,
        auth: &AuthContext,
        request: proto::InviteToChatRequest,
    ) -> Result<response::Result, ServerError> {
        rchat_proto::validate::chat_id(&request.chat_id)
            .map_err(|e| ServerError::invalid_request(e.to_string()))?;
        let (chat, members) = self
            .repo
            .get_chat(&request.chat_id)
            .await?
            .ok_or_else(|| ServerError::not_found("chat not found"))?;
        if chat.kind != proto::ChatKind::Group as i32 {
            return Err(ServerError::invalid_request(
                "membership management applies to group chats only",
            ));
        }
        if !members.iter().any(|m| m.user_id == auth.user.user_id) {
            return Err(ServerError::forbidden("not a member of this chat"));
        }
        if request.usernames.len() > rchat_proto::limits::MAX_INVITES_PER_REQUEST {
            return Err(ServerError::limit_exceeded(
                "too many members in one request",
            ));
        }
        let existing: BTreeSet<String> = members.iter().map(|m| m.user_id.clone()).collect();
        let mut new_users: Vec<UserRecord> = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for raw in &request.usernames {
            let username = validate_username(raw)?;
            let user = self
                .repo
                .find_user_by_username(&username)
                .await?
                .ok_or_else(|| ServerError::not_found("user not found"))?;
            if user.user_id == auth.user.user_id
                || existing.contains(&user.user_id)
                || !seen.insert(user.user_id.clone())
            {
                continue;
            }
            new_users.push(user);
        }

        let now = rchat_proto::now_ms();
        if new_users.is_empty() {
            // Идемпотентно: все уже в чате.
            let users = self
                .member_users(&members, std::slice::from_ref(&auth.user))
                .await?;
            return Ok(response::Result::InviteToChat(
                proto::InviteToChatResponse {
                    chat: Some(p::chat_to_proto(&chat, &members, &users)),
                },
            ));
        }
        if chat.e2e_mode == proto::E2eMode::OlmV1 as i32 {
            let ids: Vec<String> = new_users.iter().map(|u| u.user_id.clone()).collect();
            self.require_e2e_for_users(&ids).await?;
        }
        let max_members = self.group_max_members();
        if members.len() + new_users.len() > max_members {
            return Err(ServerError::limit_exceeded(
                "group chat member limit reached",
            ));
        }

        let new_members: Vec<MemberRecord> = new_users
            .iter()
            .map(|user| MemberRecord {
                chat_id: chat.chat_id.clone(),
                user_id: user.user_id.clone(),
                joined_at_ms: now,
                role: proto::ChatMemberRole::Member as i32,
            })
            .collect();
        let mut members_after = members.clone();
        members_after.extend(new_members.iter().cloned());
        let chat_after = Self::chat_after_update(&chat, now);
        let users = self.member_users(&members_after, &new_users).await?;
        let chat_proto = p::chat_to_proto(&chat_after, &members_after, &users);
        let targets: Vec<String> = members_after.iter().map(|m| m.user_id.clone()).collect();
        let (events, devices, payload) =
            self.chat_updated_fanout(&chat_proto, &targets, now).await?;
        let seqs = self
            .repo
            .add_chat_members(&chat.chat_id, &new_members, max_members, &events, now)
            .await?;
        self.push_chat_events(&devices, &payload, &seqs);

        Ok(response::Result::InviteToChat(
            proto::InviteToChatResponse {
                chat: Some(chat_proto),
            },
        ))
    }

    async fn handle_leave_chat(
        &self,
        auth: &AuthContext,
        request: proto::LeaveChatRequest,
    ) -> Result<response::Result, ServerError> {
        rchat_proto::validate::chat_id(&request.chat_id)
            .map_err(|e| ServerError::invalid_request(e.to_string()))?;
        let (chat, members) = self
            .repo
            .get_chat(&request.chat_id)
            .await?
            .ok_or_else(|| ServerError::not_found("chat not found"))?;
        if chat.kind != proto::ChatKind::Group as i32 {
            return Err(ServerError::invalid_request(
                "leaving is supported for group chats only",
            ));
        }
        let me = members
            .iter()
            .find(|m| m.user_id == auth.user.user_id)
            .ok_or_else(|| ServerError::forbidden("not a member of this chat"))?
            .clone();
        // Владелец передаёт владение участнику с самым ранним вступлением.
        let new_owner = if me.is_owner() {
            members
                .iter()
                .filter(|m| m.user_id != auth.user.user_id)
                .min_by(|a, b| (a.joined_at_ms, &a.user_id).cmp(&(b.joined_at_ms, &b.user_id)))
                .map(|m| m.user_id.clone())
        } else {
            None
        };
        let now = rchat_proto::now_ms();
        let mut members_after: Vec<MemberRecord> = members
            .iter()
            .filter(|m| m.user_id != auth.user.user_id)
            .cloned()
            .collect();
        if let Some(owner) = &new_owner {
            for member in &mut members_after {
                if &member.user_id == owner {
                    member.role = proto::ChatMemberRole::Owner as i32;
                }
            }
        }
        let chat_after = Self::chat_after_update(&chat, now);
        let users = self
            .member_users(&members_after, std::slice::from_ref(&auth.user))
            .await?;
        let chat_proto = p::chat_to_proto(&chat_after, &members_after, &users);
        let mut targets: Vec<String> = members_after.iter().map(|m| m.user_id.clone()).collect();
        targets.push(auth.user.user_id.clone());
        let (events, devices, payload) =
            self.chat_updated_fanout(&chat_proto, &targets, now).await?;
        let seqs = self
            .repo
            .remove_chat_member(
                &chat.chat_id,
                &auth.user.user_id,
                new_owner.as_deref(),
                &events,
                now,
            )
            .await?;
        self.push_chat_events(&devices, &payload, &seqs);

        Ok(response::Result::LeaveChat(proto::LeaveChatResponse {}))
    }

    async fn handle_remove_chat_member(
        &self,
        auth: &AuthContext,
        request: proto::RemoveChatMemberRequest,
    ) -> Result<response::Result, ServerError> {
        rchat_proto::validate::chat_id(&request.chat_id)
            .map_err(|e| ServerError::invalid_request(e.to_string()))?;
        rchat_proto::validate::user_id(&request.user_id)
            .map_err(|e| ServerError::invalid_request(e.to_string()))?;
        let (chat, members) = self
            .repo
            .get_chat(&request.chat_id)
            .await?
            .ok_or_else(|| ServerError::not_found("chat not found"))?;
        if chat.kind != proto::ChatKind::Group as i32 {
            return Err(ServerError::invalid_request(
                "membership management applies to group chats only",
            ));
        }
        let me = members
            .iter()
            .find(|m| m.user_id == auth.user.user_id)
            .ok_or_else(|| ServerError::forbidden("not a member of this chat"))?;
        if !me.is_owner() {
            return Err(ServerError::forbidden("only the owner can remove members"));
        }
        if request.user_id == auth.user.user_id {
            return Err(ServerError::invalid_request("use leave to quit the chat"));
        }
        if !members.iter().any(|m| m.user_id == request.user_id) {
            return Err(ServerError::not_found("member not found"));
        }
        let now = rchat_proto::now_ms();
        let members_after: Vec<MemberRecord> = members
            .iter()
            .filter(|m| m.user_id != request.user_id)
            .cloned()
            .collect();
        let chat_after = Self::chat_after_update(&chat, now);
        let users = self
            .member_users(&members_after, std::slice::from_ref(&auth.user))
            .await?;
        let chat_proto = p::chat_to_proto(&chat_after, &members_after, &users);
        let mut targets: Vec<String> = members_after.iter().map(|m| m.user_id.clone()).collect();
        targets.push(request.user_id.clone());
        let (events, devices, payload) =
            self.chat_updated_fanout(&chat_proto, &targets, now).await?;
        let seqs = self
            .repo
            .remove_chat_member(&chat.chat_id, &request.user_id, None, &events, now)
            .await?;
        self.push_chat_events(&devices, &payload, &seqs);

        Ok(response::Result::RemoveChatMember(
            proto::RemoveChatMemberResponse {
                chat: Some(chat_proto),
            },
        ))
    }

    async fn handle_rename_chat(
        &self,
        auth: &AuthContext,
        request: proto::RenameChatRequest,
    ) -> Result<response::Result, ServerError> {
        rchat_proto::validate::chat_id(&request.chat_id)
            .map_err(|e| ServerError::invalid_request(e.to_string()))?;
        let name = validate_group_name(&request.name)?;
        let (chat, members) = self
            .repo
            .get_chat(&request.chat_id)
            .await?
            .ok_or_else(|| ServerError::not_found("chat not found"))?;
        if chat.kind != proto::ChatKind::Group as i32 {
            return Err(ServerError::invalid_request(
                "renaming applies to group chats only",
            ));
        }
        let me = members
            .iter()
            .find(|m| m.user_id == auth.user.user_id)
            .ok_or_else(|| ServerError::forbidden("not a member of this chat"))?;
        if !me.is_owner() {
            return Err(ServerError::forbidden("only the owner can rename the chat"));
        }
        if chat.name == name {
            let users = self
                .member_users(&members, std::slice::from_ref(&auth.user))
                .await?;
            return Ok(response::Result::RenameChat(proto::RenameChatResponse {
                chat: Some(p::chat_to_proto(&chat, &members, &users)),
            }));
        }
        let now = rchat_proto::now_ms();
        let mut chat_after = Self::chat_after_update(&chat, now);
        chat_after.name = name;
        let users = self
            .member_users(&members, std::slice::from_ref(&auth.user))
            .await?;
        let chat_proto = p::chat_to_proto(&chat_after, &members, &users);
        let targets: Vec<String> = members.iter().map(|m| m.user_id.clone()).collect();
        let (events, devices, payload) =
            self.chat_updated_fanout(&chat_proto, &targets, now).await?;
        let seqs = self
            .repo
            .rename_chat(&chat.chat_id, &chat_after.name, &events, now)
            .await?;
        self.push_chat_events(&devices, &payload, &seqs);

        Ok(response::Result::RenameChat(proto::RenameChatResponse {
            chat: Some(chat_proto),
        }))
    }

    // -----------------------------------------------------------------
    // Сигнализация звонков (медиа нет)
    // -----------------------------------------------------------------

    async fn handle_call_invite(
        &self,
        auth: &AuthContext,
        request: proto::CallInviteRequest,
    ) -> Result<response::Result, ServerError> {
        if !self.limiter.call_invite_allowed(&auth.user.user_id) {
            return Err(ServerError::rate_limited());
        }
        rchat_proto::validate::chat_id(&request.chat_id)
            .map_err(|e| ServerError::invalid_request(e.to_string()))?;
        let (chat, members) = self
            .repo
            .get_chat(&request.chat_id)
            .await?
            .ok_or_else(|| ServerError::not_found("chat not found"))?;
        if chat.kind != proto::ChatKind::Direct as i32 {
            return Err(ServerError::invalid_request(
                "calls are supported only in direct chats",
            ));
        }
        if !members.iter().any(|m| m.user_id == auth.user.user_id) {
            return Err(ServerError::forbidden("not a member of this chat"));
        }
        let callee = members
            .iter()
            .find(|m| m.user_id != auth.user.user_id)
            .ok_or_else(|| ServerError::invalid_request("direct chat has no peer"))?;
        if !self.connections.is_online(&callee.user_id) {
            return Err(ServerError::conflict("peer is offline"));
        }

        self.expire_calls();
        let call_id = random_id()?;
        let now = rchat_proto::now_ms();
        let call = self
            .calls
            .invite(
                &call_id,
                &chat.chat_id,
                &auth.user.user_id,
                &callee.user_id,
                now,
            )
            .map_err(call_error)?;
        // Соединение callee могло закрыться между проверкой и вставкой в
        // реестр; тогда звонок некому доставить. Гонку с уходом в офлайн
        // после этой проверки закрывает gateway (end_calls_for_user).
        if !self.connections.is_online(&callee.user_id) {
            let _ = self.calls.end(&call.call_id, &auth.user.user_id);
            return Err(ServerError::conflict("peer is offline"));
        }
        self.push_call_to(
            &call,
            proto::CallEventKind::Invited,
            &[&call.callee_user_id],
        );
        Ok(response::Result::CallInvite(proto::CallInviteResponse {
            call: Some(p::call_to_proto(&call)),
        }))
    }

    async fn handle_call_accept(
        &self,
        auth: &AuthContext,
        request: proto::CallAcceptRequest,
    ) -> Result<response::Result, ServerError> {
        rchat_proto::validate::call_id(&request.call_id)
            .map_err(|e| ServerError::invalid_request(e.to_string()))?;
        self.expire_calls();
        let now = rchat_proto::now_ms();
        let call = self
            .calls
            .accept(&request.call_id, &auth.user.user_id, now)
            .map_err(call_error)?;
        self.push_call(&call, proto::CallEventKind::Accepted);
        Ok(response::Result::CallAccept(proto::CallAcceptResponse {
            call: Some(p::call_to_proto(&call)),
        }))
    }

    async fn handle_call_decline(
        &self,
        auth: &AuthContext,
        request: proto::CallDeclineRequest,
    ) -> Result<response::Result, ServerError> {
        rchat_proto::validate::call_id(&request.call_id)
            .map_err(|e| ServerError::invalid_request(e.to_string()))?;
        self.expire_calls();
        let call = self
            .calls
            .decline(&request.call_id, &auth.user.user_id)
            .map_err(call_error)?;
        self.push_call(&call, proto::CallEventKind::Declined);
        Ok(response::Result::CallDecline(proto::CallDeclineResponse {
            call: Some(p::call_to_proto(&call)),
        }))
    }

    async fn handle_call_end(
        &self,
        auth: &AuthContext,
        request: proto::CallEndRequest,
    ) -> Result<response::Result, ServerError> {
        rchat_proto::validate::call_id(&request.call_id)
            .map_err(|e| ServerError::invalid_request(e.to_string()))?;
        self.expire_calls();
        let call = self
            .calls
            .end(&request.call_id, &auth.user.user_id)
            .map_err(call_error)?;
        self.push_call(&call, proto::CallEventKind::Ended);
        Ok(response::Result::CallEnd(proto::CallEndResponse {
            call: Some(p::call_to_proto(&call)),
        }))
    }

    /// Завершает звонки пользователя, ушедшего в офлайн (нет соединений).
    pub fn end_calls_for_user(&self, user_id: &str) {
        for call in self.calls.end_for_user(user_id) {
            self.push_call(&call, proto::CallEventKind::Ended);
        }
    }

    /// Удаляет истёкшие звонки и уведомляет участников (best-effort).
    fn expire_calls(&self) {
        for call in self.calls.expire(rchat_proto::now_ms()) {
            self.push_call(&call, proto::CallEventKind::Ended);
        }
    }

    /// Рассылает transient-событие звонка всем устройствам обоих участников.
    fn push_call(&self, call: &ActiveCall, kind: proto::CallEventKind) {
        self.push_call_to(call, kind, &[&call.caller_user_id, &call.callee_user_id]);
    }

    /// Рассылает transient-событие звонка указанным пользователям.
    fn push_call_to(&self, call: &ActiveCall, kind: proto::CallEventKind, users: &[&str]) {
        let envelope = p::push_call_event(call, kind, rchat_proto::now_ms());
        let frame = match rchat_proto::encode_message(&envelope) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::error!(%error, "failed to encode call event");
                return;
            }
        };
        for user_id in users {
            self.connections.deliver_user(user_id, frame.clone());
        }
    }

    // -----------------------------------------------------------------
    // Сообщения
    // -----------------------------------------------------------------

    async fn handle_send_message(
        &self,
        auth: &AuthContext,
        request: proto::SendMessageRequest,
    ) -> Result<response::Result, ServerError> {
        if !self.limiter.send_allowed(&auth.user.user_id) {
            return Err(ServerError::rate_limited());
        }
        rchat_proto::validate::chat_id(&request.chat_id)
            .map_err(|e| ServerError::invalid_request(e.to_string()))?;
        rchat_proto::validate::client_message_id(&request.client_message_id)
            .map_err(|e| ServerError::invalid_request(e.to_string()))?;
        let (chat, members, devices) = self.members_and_devices(&request.chat_id).await?;
        if !members.iter().any(|m| m.user_id == auth.user.user_id) {
            return Err(ServerError::forbidden("not a member of this chat"));
        }
        if request.reply_to_server_id != 0 {
            let parent = self
                .repo
                .get_message(request.reply_to_server_id)
                .await?
                .ok_or_else(|| ServerError::invalid_request("reply target not found"))?;
            if parent.chat_id != chat.chat_id {
                return Err(ServerError::invalid_request(
                    "reply target belongs to another chat",
                ));
            }
        }

        let now = rchat_proto::now_ms();
        let server_id = self.next_message_id();
        let content = self.prepare_outgoing(
            &chat,
            &devices,
            &auth.device.device_id,
            server_id,
            &request.content_kind,
            &request.recipients,
            &request.plaintext_dev,
            &request.group_ciphertext,
            &request.group_session_id,
        )?;

        let message = MessageRecord {
            server_id,
            chat_id: chat.chat_id.clone(),
            sender_user_id: auth.user.user_id.clone(),
            sender_device_id: auth.device.device_id.clone(),
            client_message_id: request.client_message_id.clone(),
            content_kind: content.content_kind.clone(),
            reply_to_server_id: request.reply_to_server_id,
            created_at_ms: now,
            edited_at_ms: 0,
            deleted_at_ms: 0,
            sender_curve25519: auth.device.curve25519.clone(),
            olm_type: 0,
            plaintext_dev: content.plaintext.clone(),
            group_ciphertext: content.group_ciphertext.clone(),
            group_session_id: content.group_session_id.clone(),
            grouped_id: request.grouped_id.clone(),
        };

        // События по одному на каждое неотозванное устройство участника.
        let mut events = Vec::with_capacity(devices.len());
        for device in &devices {
            let recipient = content
                .recipients
                .iter()
                .find(|r| r.device_id == device.device_id);
            let view = p::message_view(&message, recipient, chat.e2e_mode);
            let payload = p::encode_event_payload(p::message_event(view))?;
            events.push(NewSyncEvent {
                device_id: device.device_id.clone(),
                kind_hint: 11,
                payload,
                created_at_ms: now,
            });
        }

        let insert = NewMessage {
            message: message.clone(),
            recipients: content.recipients.clone(),
            events,
        };
        match self.repo.insert_message(&insert).await? {
            InsertMessageOutcome::Inserted {
                message,
                event_seqs,
            } => {
                for (index, device) in devices.iter().enumerate() {
                    if let Some((_, seq)) = event_seqs.get(index) {
                        let recipient = content
                            .recipients
                            .iter()
                            .find(|r| r.device_id == device.device_id);
                        let view = p::message_view(&message, recipient, chat.e2e_mode);
                        let payload = p::encode_event_payload(p::message_event(view))?;
                        self.push_event(&device.user_id, &device.device_id, &payload, *seq);
                    }
                }
                let view = p::message_view(&message, None, chat.e2e_mode);
                Ok(response::Result::SendMessage(proto::SendMessageResponse {
                    message: Some(view),
                }))
            }
            InsertMessageOutcome::Duplicate(existing) => {
                // Идемпотентный повтор: возвращаем результат первой операции.
                let recipient = self
                    .repo
                    .message_recipient(existing.server_id, &auth.device.device_id)
                    .await?;
                let view = p::message_view(&existing, recipient.as_ref(), chat.e2e_mode);
                Ok(response::Result::SendMessage(proto::SendMessageResponse {
                    message: Some(view),
                }))
            }
        }
    }

    /// Общая проверка содержимого для отправки/редактирования:
    /// режим чата, покрытие устройств, размеры, валидация recipients.
    ///
    /// Личный E2E-чат: ciphertext-ы на все устройства (полное покрытие),
    /// group-поля запрещены. Групповой E2E-чат: общий megolm-ciphertext
    /// обязателен, recipients — только доставка ключа (payload_kind =
    /// GROUP_KEY), полное покрытие не требуется.
    #[allow(clippy::too_many_arguments)]
    fn prepare_outgoing(
        &self,
        chat: &ChatRecord,
        devices: &[DeviceRecord],
        sender_device_id: &str,
        server_id: u64,
        content_kind: &str,
        recipients_in: &[proto::RecipientPayload],
        plaintext_dev: &str,
        group_ciphertext: &[u8],
        group_session_id: &str,
    ) -> Result<OutgoingContent, ServerError> {
        rchat_proto::validate::content_kind(content_kind)
            .map_err(|e| ServerError::invalid_request(e.to_string()))?;
        if recipients_in.len() > rchat_proto::limits::MAX_RECIPIENTS_PER_MESSAGE {
            return Err(ServerError::invalid_request("too many recipients"));
        }
        let chat_kind = proto::ChatKind::try_from(chat.kind)
            .map_err(|_| ServerError::internal("chat has unknown kind"))?;
        let is_group = chat_kind == proto::ChatKind::Group;
        let member_ids: BTreeSet<&str> = devices.iter().map(|d| d.device_id.as_str()).collect();
        let e2e_ids: BTreeSet<&str> = devices
            .iter()
            .filter(|d| d.has_e2e_keys())
            .map(|d| d.device_id.as_str())
            .collect();
        let max_bytes = self.config.max_message_bytes as usize;

        let mut seen = BTreeSet::new();
        let mut recipients = Vec::with_capacity(recipients_in.len());
        for payload in recipients_in {
            if !seen.insert(payload.device_id.clone()) {
                return Err(ServerError::invalid_request("duplicate recipient device"));
            }
            if payload.device_id == sender_device_id {
                return Err(ServerError::invalid_request(
                    "recipients must not include the sending device",
                ));
            }
            if !member_ids.contains(payload.device_id.as_str()) {
                return Err(ServerError::invalid_request(
                    "recipient is not a device of a chat member",
                ));
            }
            if payload.ciphertext.is_empty() || payload.ciphertext.len() > max_bytes {
                return Err(ServerError::payload_too_large(
                    "recipient ciphertext is empty or too large",
                ));
            }
            if proto::OlmMessageType::try_from(payload.olm_type).is_err() {
                return Err(ServerError::invalid_request("unknown olm message type"));
            }
            let payload_kind = proto::RecipientPayloadKind::try_from(payload.payload_kind)
                .map_err(|_| ServerError::invalid_request("unknown recipient payload kind"))?;
            match (is_group, payload_kind) {
                (true, proto::RecipientPayloadKind::GroupKey)
                | (false, proto::RecipientPayloadKind::Message) => {}
                (true, _) => {
                    return Err(ServerError::invalid_request(
                        "group chat recipients must carry a group key",
                    ));
                }
                (false, _) => {
                    return Err(ServerError::invalid_request(
                        "group key payload is not allowed in a direct chat",
                    ));
                }
            }
            recipients.push(RecipientRecord {
                server_id,
                device_id: payload.device_id.clone(),
                ciphertext: payload.ciphertext.clone(),
                e2e_version: payload.e2e_version,
                olm_type: payload.olm_type,
                sender_curve25519: payload.sender_curve25519.clone(),
                payload_kind: payload.payload_kind,
            });
        }

        let e2e_mode = proto::E2eMode::try_from(chat.e2e_mode)
            .map_err(|_| ServerError::internal("chat has unknown e2e mode"))?;
        let (plaintext, group) = match e2e_mode {
            proto::E2eMode::NoneDev => {
                if !group_ciphertext.is_empty() || !group_session_id.is_empty() {
                    return Err(ServerError::invalid_request(
                        "group ciphertext is not allowed in a dev plaintext chat",
                    ));
                }
                if plaintext_dev.is_empty() {
                    return Err(ServerError::invalid_request(
                        "plaintext is required in a dev plaintext chat",
                    ));
                }
                if plaintext_dev.len() > max_bytes {
                    return Err(ServerError::payload_too_large("plaintext is too large"));
                }
                (Some(plaintext_dev.to_string()), None)
            }
            proto::E2eMode::OlmV1 => {
                if !plaintext_dev.is_empty() {
                    return Err(ServerError::e2e_required(
                        "plaintext is not allowed in an e2e chat",
                    ));
                }
                if is_group {
                    if group_ciphertext.is_empty() {
                        return Err(ServerError::invalid_request(
                            "group ciphertext is required in a group e2e chat",
                        ));
                    }
                    if group_ciphertext.len() > max_bytes {
                        return Err(ServerError::payload_too_large(
                            "group ciphertext is too large",
                        ));
                    }
                    let sid_len = group_session_id.chars().count();
                    if sid_len == 0 || sid_len > rchat_proto::limits::GROUP_SESSION_ID_MAX {
                        return Err(ServerError::invalid_request(
                            "group session id is empty or too long",
                        ));
                    }
                    // Полное покрытие не требуется: recipients — только
                    // доставка ключа тем устройствам, у кого его ещё нет.
                    for payload in &recipients {
                        if !e2e_ids.contains(payload.device_id.as_str()) {
                            return Err(ServerError::invalid_request(
                                "recipient device has no e2e keys",
                            ));
                        }
                        if payload.e2e_version != rchat_proto::E2E_VERSION_OLM_V1 {
                            return Err(ServerError::invalid_request("unknown e2e version"));
                        }
                        if payload.sender_curve25519.len() != rchat_proto::limits::DEVICE_KEY_LEN {
                            return Err(ServerError::invalid_request(
                                "sender curve25519 key must be 32 bytes",
                            ));
                        }
                    }
                    (None, Some(group_ciphertext.to_vec()))
                } else {
                    if !group_ciphertext.is_empty() || !group_session_id.is_empty() {
                        return Err(ServerError::invalid_request(
                            "group ciphertext is not allowed in a direct chat",
                        ));
                    }
                    // Обязательное покрытие: все E2E-устройства участников,
                    // кроме отправляющего.
                    let required: Vec<&str> = e2e_ids
                        .iter()
                        .copied()
                        .filter(|id| *id != sender_device_id)
                        .collect();
                    let missing: Vec<&str> = required
                        .iter()
                        .copied()
                        .filter(|id| !seen.contains(*id))
                        .collect();
                    if !missing.is_empty() {
                        return Err(ServerError::e2e_keys_missing(format!(
                            "encrypted copies are required for {} device(s)",
                            missing.len()
                        )));
                    }
                    // Ciphertext допустим только для E2E-устройств.
                    for payload in &recipients {
                        if !e2e_ids.contains(payload.device_id.as_str()) {
                            return Err(ServerError::invalid_request(
                                "recipient device has no e2e keys",
                            ));
                        }
                        if payload.e2e_version != rchat_proto::E2E_VERSION_OLM_V1 {
                            return Err(ServerError::invalid_request("unknown e2e version"));
                        }
                        if payload.sender_curve25519.len() != rchat_proto::limits::DEVICE_KEY_LEN {
                            return Err(ServerError::invalid_request(
                                "sender curve25519 key must be 32 bytes",
                            ));
                        }
                    }
                    (None, None)
                }
            }
            proto::E2eMode::Unspecified => {
                return Err(ServerError::internal("chat has unspecified e2e mode"))
            }
        };

        Ok(OutgoingContent {
            content_kind: content_kind.to_string(),
            recipients,
            plaintext,
            group_ciphertext: group,
            group_session_id: if group_ciphertext.is_empty() {
                String::new()
            } else {
                group_session_id.to_string()
            },
        })
    }

    async fn handle_get_history(
        &self,
        auth: &AuthContext,
        request: proto::GetHistoryRequest,
    ) -> Result<response::Result, ServerError> {
        let (chat, members, _) = self.members_and_devices(&request.chat_id).await?;
        if !members.iter().any(|m| m.user_id == auth.user.user_id) {
            return Err(ServerError::forbidden("not a member of this chat"));
        }
        let limit = if request.limit == 0 {
            self.config.max_history_page
        } else {
            request.limit.min(self.config.max_history_page).max(1)
        };
        let rows = self
            .repo
            .list_history(&chat.chat_id, request.before_server_id, limit)
            .await?;
        let has_more = rows.len() as u32 > limit;
        let rows: Vec<MessageRecord> = rows.into_iter().take(limit as usize).collect();
        let server_ids: Vec<u64> = rows.iter().map(|m| m.server_id).collect();
        let recipients = self
            .repo
            .recipients_for_device(&server_ids, &auth.device.device_id)
            .await?;
        let messages = rows
            .iter()
            .map(|message| {
                let recipient = recipients.iter().find(|r| r.server_id == message.server_id);
                p::message_view(message, recipient, chat.e2e_mode)
            })
            .collect();
        Ok(response::Result::GetHistory(proto::GetHistoryResponse {
            messages,
            has_more,
        }))
    }

    async fn handle_edit_message(
        &self,
        auth: &AuthContext,
        request: proto::EditMessageRequest,
    ) -> Result<response::Result, ServerError> {
        let message = self
            .repo
            .get_message(request.server_id)
            .await?
            .ok_or_else(|| ServerError::not_found("message not found"))?;
        let (chat, members, devices) = self.members_and_devices(&message.chat_id).await?;
        if !members.iter().any(|m| m.user_id == auth.user.user_id) {
            return Err(ServerError::forbidden("not a member of this chat"));
        }
        if message.sender_user_id != auth.user.user_id {
            return Err(ServerError::forbidden("only the author can edit a message"));
        }
        if message.deleted_at_ms != 0 {
            return Err(ServerError::conflict("message has been deleted"));
        }
        let now = rchat_proto::now_ms();
        let content = self.prepare_outgoing(
            &chat,
            &devices,
            &auth.device.device_id,
            message.server_id,
            &request.content_kind,
            &request.recipients,
            &request.plaintext_dev,
            &request.group_ciphertext,
            &request.group_session_id,
        )?;

        // Если у устройства-редактора есть E2E-ключи, сообщение после правки
        // атрибутируется ему (ciphertext-ы создал именно он).
        let sender_curve25519 = if auth.device.curve25519.is_empty() {
            None
        } else {
            Some(auth.device.curve25519.clone())
        };
        let updated_preview = MessageRecord {
            content_kind: content.content_kind.clone(),
            edited_at_ms: now,
            plaintext_dev: content.plaintext.clone(),
            group_ciphertext: content.group_ciphertext.clone(),
            group_session_id: content.group_session_id.clone(),
            // Правка могла прийти с другого устройства автора: события несут
            // фактического отправителя новых ciphertext-ов.
            sender_device_id: auth.device.device_id.clone(),
            sender_curve25519: auth.device.curve25519.clone(),
            ..message.clone()
        };
        let mut events = Vec::with_capacity(devices.len());
        for device in &devices {
            let recipient = content
                .recipients
                .iter()
                .find(|r| r.device_id == device.device_id);
            let view = p::message_view(&updated_preview, recipient, chat.e2e_mode);
            let payload = p::encode_event_payload(p::message_edited_event(view))?;
            events.push(NewSyncEvent {
                device_id: device.device_id.clone(),
                kind_hint: 12,
                payload,
                created_at_ms: now,
            });
        }

        let update = MessageUpdate {
            edited_at_ms: now,
            deleted_at_ms: 0,
            content_kind: Some(content.content_kind.clone()),
            plaintext_dev: content.plaintext.clone(),
            group_ciphertext: content.group_ciphertext.clone(),
            group_session_id: if content.group_ciphertext.is_some() {
                Some(content.group_session_id.clone())
            } else {
                None
            },
            clear_content: false,
            sender_device_id: Some(auth.device.device_id.clone()),
            sender_curve25519: sender_curve25519.clone(),
            recipients: Some(content.recipients.clone()),
            events,
            editor_user_id: auth.user.user_id.clone(),
        };
        let (updated, seqs) = self.repo.update_message(message.server_id, &update).await?;
        for (index, device) in devices.iter().enumerate() {
            if let Some((_, seq)) = seqs.get(index) {
                let recipient = content
                    .recipients
                    .iter()
                    .find(|r| r.device_id == device.device_id);
                let view = p::message_view(&updated, recipient, chat.e2e_mode);
                let payload = p::encode_event_payload(p::message_edited_event(view))?;
                self.push_event(&device.user_id, &device.device_id, &payload, *seq);
            }
        }
        let recipient = self
            .repo
            .message_recipient(updated.server_id, &auth.device.device_id)
            .await?;
        let view = p::message_view(&updated, recipient.as_ref(), chat.e2e_mode);
        Ok(response::Result::EditMessage(proto::EditMessageResponse {
            message: Some(view),
        }))
    }

    async fn handle_delete_message(
        &self,
        auth: &AuthContext,
        request: proto::DeleteMessageRequest,
    ) -> Result<response::Result, ServerError> {
        let message = self
            .repo
            .get_message(request.server_id)
            .await?
            .ok_or_else(|| ServerError::not_found("message not found"))?;
        let (chat, members, devices) = self.members_and_devices(&message.chat_id).await?;
        if !members.iter().any(|m| m.user_id == auth.user.user_id) {
            return Err(ServerError::forbidden("not a member of this chat"));
        }
        if message.sender_user_id != auth.user.user_id {
            return Err(ServerError::forbidden(
                "only the author can delete a message",
            ));
        }
        if message.deleted_at_ms != 0 {
            // Идемпотентно: уже удалено.
            let view = p::message_view(&message, None, chat.e2e_mode);
            return Ok(response::Result::DeleteMessage(
                proto::DeleteMessageResponse {
                    message: Some(view),
                },
            ));
        }
        let now = rchat_proto::now_ms();
        // Событие-надгробие не должно нести содержимое: plaintext и megolm-
        // ciphertext сбрасываются до сборки payload (иначе dev-чат утёк бы
        // текстом, а групповой — шифротекстом, в журнал).
        let deleted_preview = MessageRecord {
            deleted_at_ms: now,
            plaintext_dev: None,
            group_ciphertext: None,
            group_session_id: String::new(),
            ..message.clone()
        };
        let mut events = Vec::with_capacity(devices.len());
        for device in &devices {
            let view = p::message_view(&deleted_preview, None, chat.e2e_mode);
            let payload = p::encode_event_payload(p::message_deleted_event(view))?;
            events.push(NewSyncEvent {
                device_id: device.device_id.clone(),
                kind_hint: 13,
                payload,
                created_at_ms: now,
            });
        }
        let update = MessageUpdate {
            edited_at_ms: 0,
            deleted_at_ms: now,
            content_kind: None,
            plaintext_dev: None,
            group_ciphertext: None,
            group_session_id: None,
            // Удаление для всех: ciphertext-ы и plaintext стираются, чтобы
            // содержимое нельзя было получить ни через историю, ни из БД.
            clear_content: true,
            sender_device_id: None,
            sender_curve25519: None,
            recipients: Some(Vec::new()),
            events,
            editor_user_id: auth.user.user_id.clone(),
        };
        let (deleted, seqs) = self.repo.update_message(message.server_id, &update).await?;
        for (index, device) in devices.iter().enumerate() {
            if let Some((_, seq)) = seqs.get(index) {
                let view = p::message_view(&deleted, None, chat.e2e_mode);
                let payload = p::encode_event_payload(p::message_deleted_event(view))?;
                self.push_event(&device.user_id, &device.device_id, &payload, *seq);
            }
        }
        let view = p::message_view(&deleted, None, chat.e2e_mode);
        Ok(response::Result::DeleteMessage(
            proto::DeleteMessageResponse {
                message: Some(view),
            },
        ))
    }

    // -----------------------------------------------------------------
    // Файлы (блобы): сервер принимает и отдаёт шифротекст, не зная ключа.
    // -----------------------------------------------------------------
    async fn handle_upload_blob(
        &self,
        auth: &AuthContext,
        request: proto::UploadBlobRequest,
    ) -> Result<response::Result, ServerError> {
        let (_, members, _) = self.members_and_devices(&request.chat_id).await?;
        if !members.iter().any(|m| m.user_id == auth.user.user_id) {
            return Err(ServerError::forbidden("not a member of this chat"));
        }
        if request.blob_id.is_empty() {
            return Err(ServerError::invalid_request("blob_id is required"));
        }
        let now = rchat_proto::now_ms();
        self.repo
            .begin_blob(
                &request.blob_id,
                &request.chat_id,
                &auth.user.user_id,
                request.total_size as u64,
                now,
            )
            .await?;
        let received = self
            .repo
            .write_blob_part(&request.blob_id, request.offset, &request.data, now)
            .await?;
        Ok(response::Result::UploadBlob(proto::UploadBlobResponse {
            received,
        }))
    }

    async fn handle_finish_blob(
        &self,
        auth: &AuthContext,
        request: proto::FinishBlobRequest,
    ) -> Result<response::Result, ServerError> {
        let (_, members, _) = self.members_and_devices(&request.chat_id).await?;
        if !members.iter().any(|m| m.user_id == auth.user.user_id) {
            return Err(ServerError::forbidden("not a member of this chat"));
        }
        let now = rchat_proto::now_ms();
        let token = self
            .repo
            .finish_blob(&request.blob_id, request.total_size as u64, now)
            .await?;
        Ok(response::Result::FinishBlob(proto::FinishBlobResponse {
            blob_ref: token,
            size: request.total_size,
        }))
    }

    async fn handle_download_blob(
        &self,
        auth: &AuthContext,
        request: proto::DownloadBlobRequest,
    ) -> Result<response::Result, ServerError> {
        let Some((data, total_size, eof)) = self
            .repo
            .read_blob(&request.blob_ref, request.offset, request.length)
            .await?
        else {
            return Err(ServerError::not_found("blob not found"));
        };
        // Скачать файл может только участник чата, к которому файл привязан.
        let blob_chat = self.repo.blob_chat(&request.blob_ref).await?;
        if let Some(chat_id) = blob_chat {
            let (_, members, _) = self.members_and_devices(&chat_id).await?;
            if !members.iter().any(|m| m.user_id == auth.user.user_id) {
                return Err(ServerError::forbidden("not a member of this chat"));
            }
        }
        Ok(response::Result::DownloadBlob(
            proto::DownloadBlobResponse {
                data,
                total_size: total_size as u32,
                eof,
            },
        ))
    }

    async fn handle_abort_blob(
        &self,
        auth: &AuthContext,
        request: proto::AbortBlobRequest,
    ) -> Result<response::Result, ServerError> {
        self.repo
            .abort_blob(&request.blob_id, &auth.user.user_id)
            .await?;
        Ok(response::Result::AbortBlob(proto::AbortBlobResponse {}))
    }

    async fn handle_mark_receipt(
        &self,
        auth: &AuthContext,
        request: proto::MarkReceiptRequest,
    ) -> Result<response::Result, ServerError> {
        let (chat, members, _) = self.members_and_devices(&request.chat_id).await?;
        if !members.iter().any(|m| m.user_id == auth.user.user_id) {
            return Err(ServerError::forbidden("not a member of this chat"));
        }
        if request.up_to_server_id > chat.last_server_id {
            return Err(ServerError::invalid_request(
                "receipt points past the last message of the chat",
            ));
        }
        let now = rchat_proto::now_ms();
        let receipt = ReceiptRecord {
            chat_id: chat.chat_id.clone(),
            user_id: auth.user.user_id.clone(),
            up_to_server_id: request.up_to_server_id,
            read: request.read,
            updated_at_ms: now,
        };
        // События для устройств других участников.
        let peer_ids: Vec<String> = members
            .iter()
            .map(|m| m.user_id.clone())
            .filter(|id| *id != auth.user.user_id)
            .collect();
        let peer_devices = self.repo.devices_for_users(&peer_ids, false).await?;
        let payload = p::encode_event_payload(p::receipt_event(&receipt))?;
        let events: Vec<NewSyncEvent> = peer_devices
            .iter()
            .map(|device| NewSyncEvent {
                device_id: device.device_id.clone(),
                kind_hint: 14,
                payload: payload.clone(),
                created_at_ms: now,
            })
            .collect();
        let (update, seqs) = self.repo.upsert_receipt(&receipt, &events).await?;
        if update.moved {
            for (index, device) in peer_devices.iter().enumerate() {
                if let Some((_, seq)) = seqs.get(index) {
                    self.push_event(&device.user_id, &device.device_id, &payload, *seq);
                }
            }
        }
        Ok(response::Result::MarkReceipt(proto::MarkReceiptResponse {}))
    }

    // -----------------------------------------------------------------
    // Синхронизация и snapshot
    // -----------------------------------------------------------------

    async fn handle_sync(
        &self,
        auth: &AuthContext,
        request: proto::SyncRequest,
    ) -> Result<response::Result, ServerError> {
        let limit = if request.limit == 0 {
            self.config.max_sync_page
        } else {
            request
                .limit
                .clamp(1, rchat_proto::limits::MAX_SYNC_PAGE_HARD)
        };
        let now = rchat_proto::now_ms();
        let fetch = self
            .repo
            .fetch_sync_events(
                &auth.device.device_id,
                request.after_seq,
                limit,
                self.config.sync_retention().as_millis() as u64,
                self.config.sync_keep_min_events,
                now,
            )
            .await?;
        if let Err(error) = self
            .repo
            .record_cursor(&auth.device.device_id, request.after_seq, now)
            .await
        {
            tracing::warn!(error = %error, "failed to record sync cursor");
        }
        let mut events = Vec::with_capacity(fetch.events.len());
        for event in &fetch.events {
            events.push(p::decode_event_payload(&event.payload, event.seq)?);
        }
        Ok(response::Result::Sync(proto::SyncEventsResponse {
            from_seq: request.after_seq.saturating_add(1),
            events,
            up_to_seq: fetch.up_to_seq,
            resync_required: fetch.resync_required,
        }))
    }

    async fn handle_get_snapshot(
        &self,
        auth: &AuthContext,
    ) -> Result<response::Result, ServerError> {
        // Сначала читаем верхнюю границу журнала, затем состояние: клиент
        // применит snapshot и продолжит строго с up_to_seq.
        let up_to_seq = self.repo.max_seq_for_device(&auth.device.device_id).await?;
        let chats = self.repo.list_chats_for_user(&auth.user.user_id).await?;
        let devices = self.repo.list_devices(&auth.user.user_id, false).await?;
        let mut user_ids: BTreeSet<String> = BTreeSet::new();
        for (_, members) in &chats {
            for member in members {
                user_ids.insert(member.user_id.clone());
            }
        }
        let user_ids: Vec<String> = user_ids.into_iter().collect();
        let users = self.repo.find_users_by_ids(&user_ids).await?;
        let chats_proto = chats
            .iter()
            .map(|(chat, members)| p::chat_to_proto(chat, members, &users))
            .collect();
        Ok(response::Result::GetSnapshot(proto::SnapshotResponse {
            up_to_seq,
            chats: chats_proto,
            devices: devices.iter().map(p::device_to_proto).collect(),
            users: users.iter().map(p::user_to_proto).collect(),
            sync_retention_seconds: self.config.sync_retention_seconds,
        }))
    }

    // -----------------------------------------------------------------
    // Доставка и presence
    // -----------------------------------------------------------------

    /// Кодирует и доставляет push-событие устройству (best-effort).
    pub(crate) fn push_event(&self, user_id: &str, device_id: &str, payload: &[u8], seq: u64) {
        match p::decode_event_payload(payload, seq) {
            Ok(event) => match rchat_proto::encode_message(&p::push_sync_event(event)) {
                Ok(frame) => {
                    self.connections.deliver_device(user_id, device_id, frame);
                }
                Err(error) => {
                    tracing::error!(error = %error, "failed to encode push frame");
                }
            },
            Err(error) => {
                tracing::error!(error = %error, device_id = %device_id, "failed to decode stored event");
            }
        }
    }

    /// Рассылает presence-событие устройству и его собеседникам.
    pub async fn broadcast_presence(&self, user_id: &str, online: bool, skip_device: Option<&str>) {
        let mut targets = match self.peers_of(user_id).await {
            Ok(peers) => peers,
            Err(error) => {
                tracing::warn!(error = %error, "failed to compute peers for presence");
                return;
            }
        };
        targets.push(user_id.to_string());
        let devices = match self.repo.devices_for_users(&targets, false).await {
            Ok(devices) => devices,
            Err(error) => {
                tracing::warn!(error = %error, "failed to load devices for presence");
                return;
            }
        };
        let now = rchat_proto::now_ms();
        let frame = match rchat_proto::encode_message(&p::push_presence(user_id, online, now)) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::error!(error = %error, "failed to encode presence");
                return;
            }
        };
        for device in devices {
            if device.user_id == user_id && Some(device.device_id.as_str()) == skip_device {
                continue;
            }
            self.connections
                .deliver_device(&device.user_id, &device.device_id, frame.clone());
        }
    }

    /// Отправляет только что подключившемуся устройству текущий срез
    /// присутствия собеседников: broadcast_presence доносит лишь изменения,
    /// а без среза клиент не узнал бы, кто был online до его подключения.
    /// Срез — best-effort (как и всё presence), не журналируется.
    pub async fn send_presence_snapshot(&self, user_id: &str, device_id: &str) {
        let peers = match self.peers_of(user_id).await {
            Ok(peers) => peers,
            Err(error) => {
                tracing::warn!(error = %error, "failed to compute peers for presence snapshot");
                return;
            }
        };
        let now = rchat_proto::now_ms();
        for peer in peers {
            let online = self.connections.is_online(&peer);
            match rchat_proto::encode_message(&p::push_presence(&peer, online, now)) {
                Ok(frame) => {
                    self.connections.deliver_device(user_id, device_id, frame);
                }
                Err(error) => {
                    tracing::error!(error = %error, "failed to encode presence snapshot");
                    return;
                }
            }
        }
    }

    /// Событие изменения состава устройств пользователя его собеседникам
    /// и другим его устройствам.
    async fn fanout_device_change(
        &self,
        user_id: &str,
        device_id: &str,
        change: proto::DeviceChangeKind,
    ) {
        let mut targets = match self.peers_of(user_id).await {
            Ok(peers) => peers,
            Err(error) => {
                tracing::warn!(error = %error, "failed to compute peers for device change");
                return;
            }
        };
        targets.push(user_id.to_string());
        let devices = match self.repo.devices_for_users(&targets, false).await {
            Ok(devices) => devices,
            Err(error) => {
                tracing::warn!(error = %error, "failed to load devices for device change");
                return;
            }
        };
        let now = rchat_proto::now_ms();
        let payload =
            match p::encode_event_payload(p::device_changed_event(user_id, device_id, change, now))
            {
                Ok(payload) => payload,
                Err(error) => {
                    tracing::error!(error = %error, "failed to encode device change event");
                    return;
                }
            };
        let mut events = Vec::new();
        for device in &devices {
            if device.user_id == user_id && device.device_id == device_id {
                continue;
            }
            events.push(NewSyncEvent {
                device_id: device.device_id.clone(),
                kind_hint: 15,
                payload: payload.clone(),
                created_at_ms: now,
            });
        }
        if events.is_empty() {
            return;
        }
        match self.repo.append_sync_events(&events).await {
            Ok(seqs) => {
                let mut index = 0;
                for device in &devices {
                    if device.user_id == user_id && device.device_id == device_id {
                        continue;
                    }
                    if let Some((_, seq)) = seqs.get(index) {
                        self.push_event(&device.user_id, &device.device_id, &payload, *seq);
                    }
                    index += 1;
                }
            }
            Err(error) => {
                tracing::warn!(error = %error, "failed to append device change events");
            }
        }
    }

    async fn peers_of(&self, user_id: &str) -> Result<Vec<String>, crate::repo::RepoError> {
        let chats = self.repo.list_chats_for_user(user_id).await?;
        let mut peers = BTreeSet::new();
        for (_, members) in chats {
            for member in members {
                if member.user_id != user_id {
                    peers.insert(member.user_id);
                }
            }
        }
        Ok(peers.into_iter().collect())
    }

    async fn device_visible_to(
        &self,
        auth: &AuthContext,
        device: &DeviceRecord,
    ) -> Result<bool, ServerError> {
        if device.user_id == auth.user.user_id {
            return Ok(true);
        }
        Ok(self
            .repo
            .has_shared_chat(&auth.user.user_id, &device.user_id)
            .await?)
    }

    // -----------------------------------------------------------------
    // Пароли, сессии, утилиты
    // -----------------------------------------------------------------

    fn new_session(&self, device_id: &str, now_ms: u64) -> (String, NewSession) {
        // 32 случайных байта → 64 hex-символа; на сервере хранится SHA-256.
        let token = rchat_proto::random_id_hex(32).unwrap_or_else(|_| String::new());
        let token_hash = sha256(token.as_bytes());
        let session = NewSession {
            token_hash,
            device_id: device_id.to_string(),
            created_at_ms: now_ms,
            expires_at_ms: now_ms + self.config.session_ttl_seconds * 1000,
        };
        (token, session)
    }

    async fn hash_password(&self, password: &str) -> Result<String, ServerError> {
        let hasher = self.hasher.clone();
        let password = password.to_string();
        tokio::task::spawn_blocking(move || hasher.hash(&password))
            .await
            .map_err(|_| ServerError::internal("password hashing task failed"))?
            .map_err(|error| {
                tracing::error!(error = %error, "password hashing failed");
                ServerError::internal("password hashing failed")
            })
    }

    async fn verify_password(&self, password: &str, phc: &str) -> Result<bool, ServerError> {
        let hasher = self.hasher.clone();
        let password = password.to_string();
        let phc = phc.to_string();
        tokio::task::spawn_blocking(move || hasher.verify(&password, &phc))
            .await
            .map_err(|_| ServerError::internal("password verification task failed"))?
            .map_err(|error| {
                tracing::error!(error = %error, "password verification failed");
                ServerError::internal("password verification failed")
            })
    }
}

/// Результат общей подготовки исходящего содержимого.
struct OutgoingContent {
    content_kind: String,
    recipients: Vec<RecipientRecord>,
    plaintext: Option<String>,
    group_ciphertext: Option<Vec<u8>>,
    group_session_id: String,
}

// ---------------------------------------------------------------------
// Валидация и конвертация входа
// ---------------------------------------------------------------------

fn validate_username(raw: &str) -> Result<String, ServerError> {
    rchat_proto::validate::username(raw).map_err(|e| ServerError::username_invalid(e.to_string()))
}

fn validate_display_name(raw: &str) -> Result<String, ServerError> {
    rchat_proto::validate::display_name(raw)
        .map_err(|e| ServerError::invalid_request(e.to_string()))
}

fn validate_group_name(raw: &str) -> Result<String, ServerError> {
    rchat_proto::validate::group_name(raw).map_err(|e| ServerError::invalid_request(e.to_string()))
}

fn validate_password(raw: &str) -> Result<(), ServerError> {
    rchat_proto::validate::password(raw).map_err(|e| ServerError::invalid_request(e.to_string()))
}

fn call_error(error: CallError) -> ServerError {
    match error {
        CallError::Busy => ServerError::conflict("user is busy"),
        CallError::NotFound => ServerError::not_found("call not found"),
        CallError::Forbidden => ServerError::forbidden("not a participant of this call"),
        CallError::NotRinging => ServerError::conflict("call is not ringing"),
    }
}

fn validate_prekey(key: &proto::PreKey) -> Result<PreKeyRecord, ServerError> {
    if key.public_key.len() != rchat_proto::limits::DEVICE_KEY_LEN {
        return Err(ServerError::invalid_request("prekey must be 32 bytes long"));
    }
    Ok(PreKeyRecord {
        key_id: key.key_id,
        public_key: key.public_key.clone(),
        fallback: false,
        created_at_ms: rchat_proto::now_ms(),
    })
}

fn validate_one_time_keys(keys: &[proto::PreKey]) -> Result<Vec<PreKeyRecord>, ServerError> {
    if keys.len() > rchat_proto::limits::MAX_ONE_TIME_KEYS_PER_REQUEST {
        return Err(ServerError::limit_exceeded("too many one-time keys"));
    }
    let mut result = Vec::with_capacity(keys.len());
    let mut seen = BTreeSet::new();
    for key in keys {
        if !seen.insert(key.key_id) {
            return Err(ServerError::invalid_request("duplicate prekey id"));
        }
        result.push(validate_prekey(key)?);
    }
    Ok(result)
}

fn new_device_from_proto(
    info: &proto::DeviceInfo,
    one_time_keys: &[proto::PreKey],
    fallback_key: Option<&proto::PreKey>,
) -> Result<NewDevice, ServerError> {
    rchat_proto::validate::device_info(info)
        .map_err(|e| ServerError::invalid_request(e.to_string()))?;
    if info.platform.chars().count() > 64 {
        return Err(ServerError::invalid_request("platform string too long"));
    }
    let keys = info.keys.clone().unwrap_or_default();
    let one_time_keys = validate_one_time_keys(one_time_keys)?;
    let fallback_key = match fallback_key {
        Some(key) => {
            let mut record = validate_prekey(key)?;
            record.fallback = true;
            Some(record)
        }
        None => None,
    };
    if info.e2e_version == rchat_proto::E2E_VERSION_NONE
        && (!one_time_keys.is_empty() || fallback_key.is_some())
    {
        return Err(ServerError::invalid_request(
            "device without e2e keys must not upload prekeys",
        ));
    }
    Ok(NewDevice {
        device_id: info.device_id.clone(),
        name: info.name.clone(),
        platform: info.platform.clone(),
        e2e_version: info.e2e_version,
        ed25519: keys.ed25519,
        curve25519: keys.curve25519,
        one_time_keys,
        fallback_key,
    })
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().to_vec()
}

fn random_id() -> Result<String, ServerError> {
    rchat_proto::random_id_hex(16)
        .map_err(|e| ServerError::internal(format!("random id failed: {e}")))
}

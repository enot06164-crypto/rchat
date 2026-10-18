//! Общие помощники интеграционных тестов сервисного слоя поверх SQLite.
//!
//! Подключается каждым тестовым файлом как `mod common;`. Часть помощников
//! может не использоваться отдельным файлом — для общей тестовой библиотеки
//! это нормально, поэтому `dead_code` здесь разрешён.

#![allow(dead_code)]

use std::sync::Arc;

use rchat_proto::proto;
use rchat_proto::proto::request::Method;
use rchat_proto::proto::response::Result as R;
use rchat_server::config::{Argon2Params, ServerConfig};
use rchat_server::password::Argon2idHasher;
use rchat_server::service::{ConnState, PeerInfo, Server};
use rchat_server_storage_sqlite::SqliteRepo;

/// Пароль всех тестовых пользователей.
pub const PASSWORD: &str = "correct horse battery staple";

/// Сервер с SQLite in-memory и патчем конфигурации.
pub async fn test_server_with(config_patch: impl FnOnce(&mut ServerConfig)) -> Server {
    let mut config = ServerConfig {
        allow_dev_plaintext_chats: true,
        ..ServerConfig::default()
    };
    config_patch(&mut config);
    // Параметры Argon2 уменьшены, чтобы тесты не тратили время на KDF.
    let hasher = Argon2idHasher::new(Argon2Params {
        memory_kib: 64,
        iterations: 1,
        parallelism: 1,
    })
    .expect("argon2 params");
    let repo = SqliteRepo::open_in_memory().expect("sqlite");
    Server::start(Arc::new(repo), config, Arc::new(hasher))
        .await
        .expect("server start")
}

/// Сервер с конфигурацией по умолчанию (кроме разрешённых dev-чатов).
pub async fn test_server() -> Server {
    test_server_with(|_| {}).await
}

/// Идентификатор тестового соединения.
pub fn peer() -> PeerInfo {
    PeerInfo {
        addr: "test-conn".to_string(),
        rate_key: "test-ip".to_string(),
    }
}

/// Устройство с опциональными E2E-ключами.
pub fn device(id: &str, keys: bool) -> proto::DeviceInfo {
    proto::DeviceInfo {
        device_id: id.to_string(),
        name: format!("device-{id}"),
        platform: "test".to_string(),
        keys: keys.then(|| proto::DeviceKeys {
            ed25519: vec![0x11; 32],
            curve25519: vec![0x22; 32],
        }),
        e2e_version: if keys { 1 } else { 0 },
        created_at_ms: 0,
        last_seen_at_ms: 0,
        revoked_at_ms: 0,
        user_id: String::new(),
    }
}

/// Вызов метода на сервисном слое.
pub async fn call(server: &Server, state: &mut ConnState, method: Method) -> R {
    server.handle_request(state, method).await
}

/// Извлекает ошибку из ответа, паникует при успехе.
pub fn expect_error(result: R) -> proto::Error {
    match result {
        R::Error(error) => error,
        other => panic!("expected error, got {other:?}"),
    }
}

/// Регистрирует пользователя и возвращает `(user_id, session_token)`.
pub async fn register(
    server: &Server,
    state: &mut ConnState,
    username: &str,
    device_id: &str,
    keys: bool,
) -> (String, String) {
    let result = call(
        server,
        state,
        Method::Register(proto::RegisterRequest {
            username: username.to_string(),
            display_name: format!("{username} display"),
            password: PASSWORD.to_string(),
            device: Some(device(device_id, keys)),
            one_time_keys: Vec::new(),
            fallback_key: None,
        }),
    )
    .await;
    match result {
        R::Register(response) => {
            assert!(!response.session_token.is_empty());
            (response.user_id, response.session_token)
        }
        other => panic!("register failed: {other:?}"),
    }
}

/// Входит вторым устройством пользователя и возвращает `(user_id, session_token)`.
pub async fn login(
    server: &Server,
    state: &mut ConnState,
    username: &str,
    device_id: &str,
    keys: bool,
) -> (String, String) {
    let result = call(
        server,
        state,
        Method::Login(proto::LoginRequest {
            username: username.to_string(),
            password: PASSWORD.to_string(),
            device: Some(device(device_id, keys)),
            one_time_keys: Vec::new(),
            fallback_key: None,
        }),
    )
    .await;
    match result {
        R::Login(response) => (response.user_id, response.session_token),
        other => panic!("login failed: {other:?}"),
    }
}

/// Создаёт личный чат и возвращает его.
pub async fn create_direct_chat(
    server: &Server,
    state: &mut ConnState,
    peer_username: &str,
    mode: proto::E2eMode,
) -> proto::Chat {
    let result = call(
        server,
        state,
        Method::CreateDirectChat(proto::CreateDirectChatRequest {
            peer_username: peer_username.to_string(),
            e2e_mode: mode as i32,
        }),
    )
    .await;
    match result {
        R::CreateDirectChat(response) => response.chat.expect("chat in response"),
        other => panic!("create chat failed: {other:?}"),
    }
}

/// Отправляет сообщение и возвращает ответ как есть.
pub async fn send_message(
    server: &Server,
    state: &mut ConnState,
    request: proto::SendMessageRequest,
) -> R {
    call(server, state, Method::SendMessage(request)).await
}

/// Извлекает отправленное сообщение из ответа, паникует при ошибке.
pub fn expect_sent(result: R) -> proto::Message {
    match result {
        R::SendMessage(response) => response.message.expect("message in response"),
        other => panic!("send failed: {other:?}"),
    }
}

/// Запрашивает страницу журнала синхронизации.
pub async fn sync(
    server: &Server,
    state: &mut ConnState,
    after_seq: u64,
) -> proto::SyncEventsResponse {
    let result = call(
        server,
        state,
        Method::Sync(proto::SyncRequest {
            after_seq,
            limit: 0,
        }),
    )
    .await;
    match result {
        R::Sync(response) => response,
        other => panic!("sync failed: {other:?}"),
    }
}

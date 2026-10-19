//! Тесты C-ABI без промежуточного языка: вызовы экспортированных функций
//! напрямую, реальный сервер из `rchat-testkit` и два независимых handle.

use std::ffi::{CStr, CString};
use std::path::Path;
use std::time::{Duration, Instant};

use rchat_ffi::{
    rchat_api_version, rchat_create, rchat_destroy, rchat_free_string, rchat_last_error,
    rchat_receive_json, rchat_send_json, RchatHandle,
};
use rchat_testkit::TestServer;
use serde_json::{json, Value};

struct Handle(*mut RchatHandle);

impl Handle {
    fn create(data_dir: &Path, server: &str, auto_start: bool) -> Self {
        let config = json!({
            "data_dir": data_dir,
            "server": server,
            "auto_start": auto_start,
        });
        let text = CString::new(config.to_string()).expect("config");
        // SAFETY: валидная NUL-терминированная строка; результат проверяется.
        let handle = unsafe { rchat_create(text.as_ptr()) };
        assert!(!handle.is_null(), "create failed: {}", last_error());
        Self(handle)
    }

    fn send(&self, request: Value) -> Value {
        let text = CString::new(request.to_string()).expect("request");
        // SAFETY: handle валиден, строка NUL-терминирована.
        let response = unsafe { rchat_send_json(self.0, text.as_ptr()) };
        assert!(!response.is_null(), "send returned NULL");
        // SAFETY: ответ создан API и ещё не освобождён.
        let value = unsafe { CStr::from_ptr(response) }
            .to_str()
            .expect("response utf-8")
            .to_string();
        // SAFETY: строка создана rchat_send_json.
        unsafe { rchat_free_string(response) };
        serde_json::from_str(&value).expect("response json")
    }

    fn ok(&self, request: Value) -> Value {
        let response = self.send(request);
        assert_eq!(response["@type"], "ok", "request failed: {response}");
        response["data"].clone()
    }

    fn receive(&self, timeout_ms: u64) -> Option<Value> {
        // SAFETY: handle валиден.
        let response = unsafe { rchat_receive_json(self.0, timeout_ms) };
        if response.is_null() {
            return None;
        }
        // SAFETY: ответ создан API и ещё не освобождён.
        let value = unsafe { CStr::from_ptr(response) }
            .to_str()
            .expect("event utf-8")
            .to_string();
        // SAFETY: строка создана rchat_receive_json.
        unsafe { rchat_free_string(response) };
        Some(serde_json::from_str(&value).expect("event json"))
    }

    fn wait_event(&self, name: &str, timeout: Duration) -> Value {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                panic!("event {name} timeout");
            }
            if let Some(event) = self.receive(remaining.as_millis().min(500) as u64) {
                if event["@type"] == "event" && event["event"] == name {
                    return event["data"].clone();
                }
            }
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: handle создан API и освобождается один раз.
        unsafe { rchat_destroy(self.0) };
    }
}

fn last_error() -> String {
    // SAFETY: возвращает заимствованную строку или NULL.
    let pointer = rchat_last_error();
    if pointer.is_null() {
        return "<no error>".to_string();
    }
    // SAFETY: указатель валиден до следующего изменения ошибки.
    unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .to_string()
}

#[test]
fn create_ping_errors_and_free() {
    let dir = tempfile::tempdir().expect("tempdir");
    let handle = Handle::create(dir.path(), "ws://127.0.0.1:1", false);

    let pong = handle.ok(json!({"@type": "ping"}));
    assert_eq!(pong["service"], "rchat-ffi");
    assert_eq!(pong["api_version"], 1);
    assert_eq!(pong["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(rchat_api_version(), 1);
    assert!(handle.receive(0).is_none());
    assert!(unsafe { rchat_receive_json(std::ptr::null_mut(), 0) }.is_null());

    let status = handle.ok(json!({"@type": "get_status"}));
    assert_eq!(status["connection"], "disconnected");
    assert!(status["account"].is_null());
    assert_eq!(status["outbox"]["total"], 0);
    assert_eq!(status["e2e"], true);

    let chats = handle.ok(json!({"@type": "get_chats"}));
    assert_eq!(chats, json!([]));

    let unknown = handle.send(json!({"@type": "no_such_method"}));
    assert_eq!(unknown["@type"], "error");
    assert_eq!(unknown["code"], "unknown_method");

    let invalid = handle.send(json!({"no_type": true}));
    assert_eq!(invalid["code"], "invalid_request");

    // Проверка NULL на входе и NULL-handle.
    assert!(unsafe { rchat_create(std::ptr::null()) }.is_null());
    assert!(!last_error().is_empty());
    let null_response = unsafe { rchat_send_json(std::ptr::null_mut(), std::ptr::null()) };
    assert!(!null_response.is_null());
    unsafe { rchat_free_string(null_response) };
}

#[test]
fn create_rejects_invalid_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cases = [
        ("not json".to_string(), "malformed config must be rejected"),
        (
            json!({
                "data_dir": dir.path(),
                "server": "ws://127.0.0.1:1",
                "tls_insecure": true,
                "tls_fingerprint": "00",
            })
            .to_string(),
            "tls_insecure and tls_fingerprint must conflict",
        ),
        (
            json!({
                "data_dir": dir.path(),
                "server": "ws://127.0.0.1:1",
                "unknown_field": true,
            })
            .to_string(),
            "unknown config field must be rejected",
        ),
        (
            json!({
                "data_dir": dir.path(),
                "server": "ws://127.0.0.1:1",
                "request_timeout_ms": 0,
            })
            .to_string(),
            "zero timeout must be rejected",
        ),
    ];
    for (config, message) in cases {
        let text = CString::new(config).expect("config");
        // SAFETY: валидная NUL-терминированная строка; результат проверяется.
        let handle = unsafe { rchat_create(text.as_ptr()) };
        assert!(handle.is_null(), "{message}");
        assert!(!last_error().is_empty(), "{message}: no last_error");
    }
}

#[test]
fn two_handles_exchange_e2e_message_and_call_event() {
    // Сервер живёт в отдельном runtime; FFI-вызовы синхронные и используют
    // собственный runtime, поэтому их нельзя делать внутри block_on.
    let server_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("server runtime");
    let server = server_runtime.block_on(TestServer::start_with(|config| {
        config.allow_dev_plaintext_chats = false;
    }));
    let url = server.ws_url();
    let root = tempfile::tempdir().expect("tempdir");
    let alice = Handle::create(&root.path().join("alice"), &url, true);
    let bob = Handle::create(&root.path().join("bob"), &url, true);

    let alice_session = alice.ok(json!({
        "@type": "register",
        "username": "alice",
        "display_name": "Alice",
        "password": "pw-alice-1",
    }));
    assert_eq!(alice_session["active"], true);
    let bob_session = bob.ok(json!({
        "@type": "register",
        "username": "bob",
        "display_name": "Bob",
        "password": "pw-bob-1",
    }));
    assert_eq!(bob_session["active"], true);

    let chat = alice.ok(json!({
        "@type": "create_direct_chat",
        "peer_username": "bob",
        "e2e": true,
    }));
    let chat_id = chat["chat_id"].as_str().expect("chat id").to_string();
    assert_eq!(chat["e2e_mode"], "olm_v1");

    // Bob может узнать о чате явным refresh (или из фоновой синхронизации).
    let bob_chats = bob.ok(json!({"@type": "refresh_chats"}));
    assert!(bob_chats
        .as_array()
        .expect("chats")
        .iter()
        .any(|chat| chat["chat_id"] == chat_id));

    let sent = alice.ok(json!({
        "@type": "send_message",
        "chat_id": chat_id,
        "text": "привет из FFI",
    }));
    assert!(sent["local_id"].as_str().is_some_and(|id| !id.is_empty()));

    // Ждём расшифрованное сообщение в локальной истории Bob. Явный sync
    // может кратко ответить not_connected во время реконнекта.
    let deadline = Instant::now() + Duration::from_secs(20);
    let message = loop {
        let _ = bob.send(json!({"@type": "sync"}));
        let messages = bob.ok(json!({
            "@type": "get_messages",
            "chat_id": chat_id,
            "limit": 50,
        }));
        if let Some(message) = messages
            .as_array()
            .expect("messages")
            .iter()
            .find(|message| message["body"] == "привет из FFI")
        {
            break message.clone();
        }
        assert!(
            Instant::now() < deadline,
            "did not receive decrypted message"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(message["status"], "none");
    assert!(message["server_id"].as_u64().is_some_and(|id| id > 0));

    // Звонок: alice приглашает, bob получает transient-событие и принимает.
    let call = alice.ok(json!({
        "@type": "call_invite",
        "chat_id": chat_id,
    }));
    let call_id = call["call_id"].as_str().expect("call id").to_string();
    let invited = bob.wait_event("call_invited", Duration::from_secs(10));
    assert_eq!(invited["call_id"], call_id);
    let accepted = bob.ok(json!({
        "@type": "call_accept",
        "call_id": call_id,
    }));
    assert_eq!(accepted["state"], "active");
    let ended = alice.ok(json!({
        "@type": "call_end",
        "call_id": call_id,
    }));
    assert_eq!(ended["state"], "active");
    bob.wait_event("call_ended", Duration::from_secs(10));

    drop(alice);
    drop(bob);
    server_runtime.block_on(server.shutdown());
}

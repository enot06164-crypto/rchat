//! Сквозные E2E-тесты (спецификация §9): два клиента с настоящим
//! провайдером Olm (vodozemac) поверх реального сервера, реального
//! WebSocket-транспорта, SQLite-хранилища клиента и шифрованного файлового
//! хранилища секретов.
//!
//! Проверяемые инварианты:
//! - сервер не получает и не хранит открытый текст E2E-сообщений;
//! - получатель расшифровывает сообщение, доставленное, пока он был офлайн,
//!   в том числе после перезапуска клиентского процесса;
//! - при невозможности E2E-согласования (нет ключей получателя) отправка
//!   отклоняется с ошибкой и не уходит открытым текстом.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rchat_client_storage_sqlite::{ClientSqliteStorage, EncryptedFileSecretStore};
use rchat_core::client::{Client, ClientBuilder};
use rchat_core::config::ClientConfig;
use rchat_core::error::CoreError;
use rchat_core::traits::Storage;
use rchat_core::types::{MessageRecord, MessageStatus};
use rchat_crypto_provider::VodozemacCryptoProvider;
use rchat_proto::proto;
use rchat_server::config::Argon2Params;
use rchat_server::{Argon2idHasher, Server, ServerConfig};
use rchat_server_storage_sqlite::net::{handle_client, ws_config};
use rchat_server_storage_sqlite::SqliteRepo;
use rchat_transport_ws::{WsTransportConfig, WsTransportFactory};
use rusqlite::Connection;
use tokio::net::TcpListener;

const PASSWORD_ALICE: &str = "pw-alice-1";
const PASSWORD_BOB: &str = "pw-bob-1";
/// Верхняя граница любого ожидания в тестах.
const TIMEOUT: Duration = Duration::from_secs(20);

/// Тестовый сервер на цикле accept; пока структура жива, порт занят,
/// а БД открыта.
struct Harness {
    port: u16,
    dir: tempfile::TempDir,
    accept: tokio::task::JoinHandle<()>,
}

impl Harness {
    fn db_path(&self) -> PathBuf {
        self.dir.path().join("server.sqlite3")
    }

    async fn shutdown(self) {
        self.accept.abort();
        let _ = self.accept.await;
    }
}

/// Поднимает сервер с собственной временной БД. Dev-plaintext-чаты
/// запрещены: тесты работают только с E2E. Параметры Argon2 уменьшены,
/// чтобы тесты не тратили время на KDF.
async fn start_server() -> Harness {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = ServerConfig {
        allow_dev_plaintext_chats: false,
        argon2: Argon2Params {
            memory_kib: 64,
            iterations: 1,
            parallelism: 1,
        },
        ..ServerConfig::default()
    };
    let repo = SqliteRepo::open(dir.path().join("server.sqlite3")).expect("repo");
    let server = Arc::new(
        Server::start(
            Arc::new(repo),
            config.clone(),
            Arc::new(Argon2idHasher::new(config.argon2).expect("hasher")),
        )
        .await
        .expect("server"),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let ws = ws_config(config.max_frame_bytes);
    let accept = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let server = Arc::clone(&server);
            tokio::spawn(async move {
                let _ = handle_client(stream, None, server, ws).await;
            });
        }
    });
    Harness { port, dir, accept }
}

/// Клиент со всеми реальными компонентами: SQLite, шифрованный файл
/// секретов, WebSocket-транспорт, провайдер Olm.
fn build_client(
    data_dir: &Path,
    port: u16,
    device_name: &str,
) -> (Client, Arc<ClientSqliteStorage>) {
    std::fs::create_dir_all(data_dir).expect("create data dir");
    let storage = Arc::new(
        ClientSqliteStorage::open(data_dir.join("client.sqlite3")).expect("client storage"),
    );
    let secrets = Arc::new(
        EncryptedFileSecretStore::open(data_dir.join("secrets.bin"), data_dir.join("secrets.key"))
            .expect("secret store"),
    );
    let transport = WsTransportFactory::new(WsTransportConfig {
        url: format!("ws://127.0.0.1:{port}"),
        ..Default::default()
    })
    .expect("transport");
    let crypto = Arc::new(VodozemacCryptoProvider::new(
        storage.clone(),
        secrets.clone(),
    ));
    let config = ClientConfig {
        device_name: device_name.to_string(),
        client_name: "rchat-e2e".to_string(),
        e2e_enabled: true,
        ..Default::default()
    };
    let client = ClientBuilder::new(config, Arc::new(transport), storage.clone(), secrets)
        .crypto_provider(crypto)
        .build()
        .expect("client");
    (client, storage)
}

/// Ждёт, пока собственное сообщение получит подтверждение сервера.
async fn wait_sent(storage: &Arc<ClientSqliteStorage>, local_id: &str) -> MessageStatus {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        if let Some(message) = storage.get_message(local_id).await.expect("get message") {
            match message.status {
                Some(MessageStatus::Sent) => return MessageStatus::Sent,
                Some(MessageStatus::Failed) => panic!("отправка {local_id} завершилась ошибкой"),
                _ => {}
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "не дождались подтверждения отправки {local_id}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Регистрация с повтором на редкое окно переподключения: соединение
/// установлено, но команда может попасть в момент краткого разрыва, и
/// клиент ответит `NotConnected`, не дойдя до сервера. Повторяем, как это
/// сделал бы пользователь.
async fn register_user(client: &Client, username: &str, display_name: &str, password: &str) {
    for attempt in 1..=5 {
        match client.register(username, display_name, password).await {
            Ok(()) => return,
            Err(CoreError::NotConnected) if attempt < 5 => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(error) => panic!("register {username}: {error}"),
        }
    }
}

/// Вход с повтором — аналогично регистрации.
async fn login_user(client: &Client, username: &str, password: &str) {
    for attempt in 1..=5 {
        match client.login(username, password).await {
            Ok(()) => return,
            Err(CoreError::NotConnected) if attempt < 5 => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(error) => panic!("login {username}: {error}"),
        }
    }
}

/// Ждёт появления расшифрованного сообщения с указанным текстом.
async fn wait_for_body(client: &Client, chat_id: &str, needle: &str) -> MessageRecord {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let messages = client
            .list_messages(chat_id, None, 50)
            .await
            .expect("list messages");
        if let Some(message) = messages
            .into_iter()
            .find(|m| m.body.as_deref() == Some(needle))
        {
            return message;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "не дождались расшифрованного сообщения {needle:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Проверяет, что во всех местах серверной БД, где может оказаться
/// содержимое сообщений, нет открытого текста. Возвращает
/// `(число сообщений, число получателей)`.
fn assert_server_has_no_plaintext(db_path: &Path, needle: &str) -> (usize, usize) {
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open server db");
    let needle_bytes = needle.as_bytes();

    // messages.plaintext_dev заполняется только для dev-plaintext-чатов
    // и в E2E-чатах обязан оставаться пустым.
    let mut stmt = conn
        .prepare("SELECT plaintext_dev FROM messages")
        .expect("prepare messages");
    let rows = stmt
        .query_map([], |row| row.get::<_, Option<String>>(0))
        .expect("query messages");
    let mut message_count = 0usize;
    for row in rows {
        let value = row.expect("row");
        message_count += 1;
        if let Some(text) = value {
            assert!(
                !text.contains(needle),
                "messages.plaintext_dev содержит открытый текст"
            );
        }
    }

    // Каждому устройству-получателю уходит только шифротекст.
    let mut stmt = conn
        .prepare("SELECT ciphertext FROM message_recipients")
        .expect("prepare recipients");
    let rows = stmt
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .expect("query recipients");
    let mut recipient_count = 0usize;
    for row in rows {
        let value = row.expect("row");
        recipient_count += 1;
        assert!(!value.is_empty(), "шифротекст получателя пуст");
        assert!(
            !contains_bytes(&value, needle_bytes),
            "message_recipients.ciphertext содержит открытый текст"
        );
    }

    // Журнал синхронизации состоит только из зашифрованных событий.
    let mut stmt = conn
        .prepare("SELECT payload FROM sync_events")
        .expect("prepare events");
    let rows = stmt
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .expect("query events");
    for row in rows {
        let value = row.expect("row");
        assert!(
            !contains_bytes(&value, needle_bytes),
            "sync_events.payload содержит открытый текст"
        );
    }

    (message_count, recipient_count)
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Типы Olm-сообщений получателей в порядке `(server_id, olm_type)`:
/// детерминированный порядок для проверок независимо от имён устройств.
fn recipient_olm_types(db_path: &Path) -> Vec<i32> {
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open server db");
    let mut stmt = conn
        .prepare("SELECT olm_type FROM message_recipients ORDER BY server_id, olm_type")
        .expect("prepare");
    let rows = stmt
        .query_map([], |row| row.get::<_, i32>(0))
        .expect("query");
    rows.map(|row| row.expect("row")).collect()
}

/// Офлайн-доставка первого E2E-сообщения (prekey), ответ (normal) и
/// проверка, что сервер нигде не видит открытый текст.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_offline_delivery_server_stores_only_ciphertext() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let alice_dir = root.path().join("alice");
    let bob_dir = root.path().join("bob");

    let (alice, alice_storage) = build_client(&alice_dir, harness.port, "alice-dev");
    alice.start().await.expect("start alice");
    register_user(&alice, "alice", "Alice", PASSWORD_ALICE).await;

    // Bob регистрируется (его ключи уходят серверу) и уходит офлайн.
    {
        let (bob, _bob_storage) = build_client(&bob_dir, harness.port, "bob-dev");
        bob.start().await.expect("start bob");
        register_user(&bob, "bob", "Bob", PASSWORD_BOB).await;
        bob.shutdown().await.expect("shutdown bob");
    }

    // E2E-чат создаётся, пока получатель офлайн.
    let chat = alice
        .create_direct_chat("bob", true)
        .await
        .expect("create e2e chat");
    assert_eq!(chat.e2e_mode, proto::E2eMode::OlmV1);

    // Первое сообщение — prekey: сессии ещё нет, получатель офлайн.
    let secret = "совершенно секретный текст 12345";
    let local_id = alice
        .send_text(&chat.chat_id, secret, None)
        .await
        .expect("send first message");
    wait_sent(&alice_storage, &local_id).await;

    let (messages, recipients) = assert_server_has_no_plaintext(&harness.db_path(), secret);
    assert_eq!(messages, 1, "на сервере должно быть одно сообщение");
    assert_eq!(recipients, 1, "шифротекст должен быть для устройства Bob");

    // Bob запускается заново с тем же локальным состоянием (аккаунт,
    // prekey-ключи, хранилище) и расшифровывает офлайн-сообщение.
    let (bob, bob_storage) = build_client(&bob_dir, harness.port, "bob-dev");
    bob.start().await.expect("start bob again");
    bob.sync_now().await.expect("sync bob");
    let received = wait_for_body(&bob, &chat.chat_id, secret).await;
    assert_eq!(received.content_kind, "text");

    // Ответ: сессия уже установлена — обычное (normal) сообщение.
    let reply = "ответ 67890";
    let reply_id = bob
        .send_text(&chat.chat_id, reply, None)
        .await
        .expect("send reply");
    wait_sent(&bob_storage, &reply_id).await;

    let (messages, recipients) = assert_server_has_no_plaintext(&harness.db_path(), reply);
    assert_eq!(messages, 2, "на сервере должно быть два сообщения");
    assert_eq!(recipients, 2, "каждому сообщению — свой шифротекст");

    // Протокол: первое сообщение prekey (1), ответ — normal (0).
    assert_eq!(recipient_olm_types(&harness.db_path()), vec![1, 0]);

    let (alice_user_id, _, _) = alice.session().expect("alice session");
    let (bob_user_id, _, _) = bob.session().expect("bob session");

    // Alice видит расшифрованный ответ.
    alice.sync_now().await.expect("sync alice");
    let for_alice = wait_for_body(&alice, &chat.chat_id, reply).await;
    assert_eq!(received.sender_user_id, alice_user_id);
    assert_eq!(for_alice.sender_user_id, bob_user_id);

    bob.shutdown().await.expect("shutdown bob");
    alice.shutdown().await.expect("shutdown alice");
    harness.shutdown().await;
}

/// Несколько устройств одного аккаунта: сообщение шифруется каждому
/// устройству получателя и каждому собственному устройству-получателю.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_multi_device_delivers_to_all_devices() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let alice1_dir = root.path().join("alice1");
    let alice2_dir = root.path().join("alice2");
    let bob_dir = root.path().join("bob");

    // Второе устройство Alice: вход в тот же аккаунт с новым устройством.
    let (alice1, alice1_storage) = build_client(&alice1_dir, harness.port, "alice-dev1");
    alice1.start().await.expect("start alice1");
    register_user(&alice1, "alice", "Alice", PASSWORD_ALICE).await;
    let (alice2, _alice2_storage) = build_client(&alice2_dir, harness.port, "alice-dev2");
    alice2.start().await.expect("start alice2");
    login_user(&alice2, "alice", PASSWORD_ALICE).await;

    let (bob, bob_storage) = build_client(&bob_dir, harness.port, "bob-dev");
    bob.start().await.expect("start bob");
    register_user(&bob, "bob", "Bob", PASSWORD_BOB).await;

    let (alice_user_id, _, _) = alice1.session().expect("alice session");

    // У Alice два устройства: чат создаёт первое, получателей — двое
    // (Bob и второе устройство Alice).
    let chat = alice1
        .create_direct_chat("bob", true)
        .await
        .expect("create e2e chat");
    let secret = "многодев. секрет 24680";
    let local_id = alice1
        .send_text(&chat.chat_id, secret, None)
        .await
        .expect("send multi-device message");
    wait_sent(&alice1_storage, &local_id).await;

    let (messages, recipients) = assert_server_has_no_plaintext(&harness.db_path(), secret);
    assert_eq!(messages, 1);
    assert_eq!(recipients, 2, "шифротекст должен уйти обоим устройствам");
    // Оба получателя получают prekey-сообщение (сессий ещё нет).
    assert_eq!(recipient_olm_types(&harness.db_path()), vec![1, 1]);

    // Bob расшифровывает.
    bob.sync_now().await.expect("sync bob");
    let received = wait_for_body(&bob, &chat.chat_id, secret).await;
    assert_eq!(received.sender_user_id, alice_user_id);

    // Второе устройство Alice тоже расшифровывает собственную копию.
    alice2.sync_now().await.expect("sync alice2");
    let own_copy = wait_for_body(&alice2, &chat.chat_id, secret).await;
    assert_eq!(own_copy.sender_user_id, alice_user_id);

    // Ответ Bob уходит обоим устройствам Alice: первому — обычное сообщение
    // (сессия уже установлена), второму — prekey (сессии ещё нет).
    let reply = "вижу оба устройства";
    let reply_id = bob
        .send_text(&chat.chat_id, reply, None)
        .await
        .expect("send reply");
    wait_sent(&bob_storage, &reply_id).await;
    let (messages, recipients) = assert_server_has_no_plaintext(&harness.db_path(), reply);
    assert_eq!(messages, 2);
    assert_eq!(recipients, 4, "ответ должен уйти двум устройствам Alice");
    assert_eq!(recipient_olm_types(&harness.db_path()), vec![1, 1, 0, 1]);

    alice1.sync_now().await.expect("sync alice1");
    let for_alice1 = wait_for_body(&alice1, &chat.chat_id, reply).await;
    let (bob_user_id, _, _) = bob.session().expect("bob session");
    assert_eq!(for_alice1.sender_user_id, bob_user_id);

    bob.shutdown().await.expect("shutdown bob");
    alice2.shutdown().await.expect("shutdown alice2");
    alice1.shutdown().await.expect("shutdown alice1");
    harness.shutdown().await;
}

/// Если у получателя не осталось E2E-устройства, отправка отклоняется
/// с ошибкой; открытый текст не уходит на сервер ни в каком виде.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_send_without_keys_is_rejected_without_plaintext_fallback() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let alice_dir = root.path().join("alice");
    let bob_dir = root.path().join("bob");

    let (alice, _alice_storage) = build_client(&alice_dir, harness.port, "alice-dev");
    alice.start().await.expect("start alice");
    register_user(&alice, "alice", "Alice", PASSWORD_ALICE).await;

    let (bob, bob_storage) = build_client(&bob_dir, harness.port, "bob-dev");
    bob.start().await.expect("start bob");
    register_user(&bob, "bob", "Bob", PASSWORD_BOB).await;

    let chat = alice
        .create_direct_chat("bob", true)
        .await
        .expect("create e2e chat");

    // Bob отзывает своё единственное E2E-устройство и уходит.
    let account = bob_storage
        .load_account()
        .await
        .expect("load account")
        .expect("account exists");
    bob.revoke_device(&account.device_id)
        .await
        .expect("revoke device");
    bob.shutdown().await.expect("shutdown bob");

    // Alice узнаёт об отзыве (событие синхронизации) и не может зашифровать.
    alice.sync_now().await.expect("sync alice");
    let error = alice
        .send_text(&chat.chat_id, "текст не должен утечь", None)
        .await
        .expect_err("отправка без E2E-устройств получателя должна отклоняться");
    match error {
        rchat_core::error::CoreError::E2eUnavailable(message) => {
            assert!(
                message.contains("no usable e2e devices"),
                "неожиданная причина: {message}"
            );
        }
        other => panic!("неожиданная ошибка: {other}"),
    }

    // На сервере нет ни сообщений чата, ни открытого текста.
    let (messages, recipients) =
        assert_server_has_no_plaintext(&harness.db_path(), "текст не должен утечь");
    assert_eq!(messages, 0, "сообщение не должно попасть на сервер");
    assert_eq!(recipients, 0, "шифротекста быть не должно");

    alice.shutdown().await.expect("shutdown alice");
    harness.shutdown().await;
}

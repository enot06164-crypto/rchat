//! Общий харнесс сквозных E2E-тестов: реальный сервер (SQLite + WebSocket)
//! из `rchat-testkit`, реальные клиенты (ядро + SQLite + шифрованное
//! хранилище секретов + провайдер Olm).
//!
//! Подключается тестовыми файлами как `mod common;`. Часть помощников может
//! не использоваться отдельным файлом — для общей тестовой библиотеки это
//! нормально, поэтому `dead_code` здесь разрешён.

#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rchat_client_storage_sqlite::{ClientSqliteStorage, EncryptedFileSecretStore};
use rchat_core::client::{Client, ClientBuilder};
use rchat_core::config::ClientConfig;
use rchat_core::error::CoreError;
use rchat_core::traits::Storage;
use rchat_core::types::{MessageRecord, MessageStatus, ReceiptRecord};
use rchat_crypto_provider::VodozemacCryptoProvider;
use rchat_testkit::TestServer;
use rchat_transport_ws::{WsTransportConfig, WsTransportFactory};
use rusqlite::Connection;

pub const PASSWORD_ALICE: &str = "pw-alice-1";
pub const PASSWORD_BOB: &str = "pw-bob-1";
/// Верхняя граница любого ожидания в тестах.
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// Поднимает сервер с собственной временной БД через `rchat-testkit`
/// (настоящий сервисный слой + SQLite, тестовые параметры Argon2).
/// Dev-plaintext-чаты запрещены: сквозные тесты работают только с E2E.
pub async fn start_server() -> TestServer {
    TestServer::start_with(|config| {
        config.allow_dev_plaintext_chats = false;
    })
    .await
}

/// Клиент со всеми реальными компонентами: SQLite, шифрованный файл
/// секретов, WebSocket-транспорт, провайдер Olm.
pub fn build_client(
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
pub async fn wait_sent(storage: &Arc<ClientSqliteStorage>, local_id: &str) -> MessageStatus {
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
pub async fn register_user(client: &Client, username: &str, display_name: &str, password: &str) {
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
pub async fn login_user(client: &Client, username: &str, password: &str) {
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
pub async fn wait_for_body(client: &Client, chat_id: &str, needle: &str) -> MessageRecord {
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

/// Ждёт, пока сообщение `server_id` придёт с признаком правки.
pub async fn wait_for_edited(client: &Client, chat_id: &str, server_id: u64) -> MessageRecord {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let messages = client
            .list_messages(chat_id, None, 50)
            .await
            .expect("list messages");
        if let Some(message) = messages
            .into_iter()
            .find(|m| m.server_id == Some(server_id) && m.edited_at_ms.is_some())
        {
            return message;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "не дождались правки сообщения #{server_id}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Ждёт, пока сообщение `server_id` будет удалено для всех (tombstone, текст
/// стёрт и локально).
pub async fn wait_for_deleted(client: &Client, chat_id: &str, server_id: u64) -> MessageRecord {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let messages = client
            .list_messages(chat_id, None, 50)
            .await
            .expect("list messages");
        if let Some(message) = messages
            .into_iter()
            .find(|m| m.server_id == Some(server_id) && m.deleted_at_ms.is_some())
        {
            assert!(
                message.body.is_none(),
                "локальное содержимое удалённого сообщения должно быть стёрто"
            );
            return message;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "не дождались удаления сообщения #{server_id}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Ждёт receipt (доставку/прочтение) от собеседника до `server_id`.
pub async fn wait_for_receipt(client: &Client, chat_id: &str, server_id: u64) -> ReceiptRecord {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let receipts = client.receipts(chat_id).await.expect("receipts");
        if let Some(receipt) = receipts
            .into_iter()
            .find(|receipt| receipt.read_up_to >= server_id)
        {
            return receipt;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "не дождались receipt до #{server_id}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Проверяет, что во всех местах серверной БД, где может оказаться
/// содержимое сообщений, нет открытого текста. Возвращает
/// `(число сообщений, число получателей)`.
pub fn assert_server_has_no_plaintext(db_path: &Path, needle: &str) -> (usize, usize) {
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

/// Ждёт, пока в чате появится не меньше `count` сообщений, и возвращает
/// последнее из них.
pub async fn wait_for_message(
    storage: &Arc<ClientSqliteStorage>,
    chat_id: &str,
    count: usize,
) -> MessageRecord {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let messages = storage
            .list_messages(chat_id, None, 100)
            .await
            .expect("list messages");
        if messages.len() >= count {
            // Список отдаётся новыми первыми — самое свежее сообщение первое.
            return messages[0].clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "не дождались {count} сообщений в чате {chat_id}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Проверяет, что сервер нигде не хранит переданные байты (содержимое
/// вложения должно ехать только внутри шифротекста).
pub fn assert_server_has_no_bytes(db_path: &Path, needle: &[u8]) {
    if needle.is_empty() {
        return;
    }
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open server db");
    // Открытый текст (только для dev-чатов) не должен содержать байты файла.
    let mut stmt = conn
        .prepare("SELECT plaintext_dev FROM messages")
        .expect("prepare messages");
    let rows = stmt
        .query_map([], |row| row.get::<_, Option<String>>(0))
        .expect("query messages");
    for row in rows {
        if let Some(text) = row.expect("row") {
            assert!(
                !contains_bytes(text.as_bytes(), needle),
                "сервер хранит содержимое вложения открытым текстом"
            );
        }
    }

    // Шифротекст получателей — тоже без открытых байтов.
    let mut stmt = conn
        .prepare("SELECT ciphertext FROM message_recipients")
        .expect("prepare recipients");
    let rows = stmt
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .expect("query recipients");
    for row in rows {
        let ciphertext = row.expect("row");
        assert!(
            !contains_bytes(&ciphertext, needle),
            "сервер хранит содержимое вложения в открытом шифротексте"
        );
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Типы Olm-сообщений получателей в порядке `(server_id, olm_type)`:
/// детерминированный порядок для проверок независимо от имён устройств.
pub fn recipient_olm_types(db_path: &Path) -> Vec<i32> {
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

/// Число ciphertext-копий сообщения в серверной БД.
pub fn message_recipient_count(db_path: &Path, server_id: u64) -> usize {
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open server db");
    conn.query_row(
        "SELECT COUNT(*) FROM message_recipients WHERE server_id = ?1",
        [server_id as i64],
        |row| row.get::<_, i64>(0),
    )
    .expect("count recipients") as usize
}

/// `plaintext_dev` сообщения в серверной БД (`None` — NULL).
pub fn server_message_plaintext(db_path: &Path, server_id: u64) -> Option<String> {
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open server db");
    conn.query_row(
        "SELECT plaintext_dev FROM messages WHERE server_id = ?1",
        [server_id as i64],
        |row| row.get::<_, Option<String>>(0),
    )
    .expect("select plaintext_dev")
}

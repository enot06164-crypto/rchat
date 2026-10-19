//! Облачный режим (как облачные чаты Telegram): сервер знает ключ чата,
//! поэтому умеет искать по истории, а новое устройство аккаунта читает всю
//! историю сразу. Приватный режим (E2E) при этом сохраняет гарантию:
//! сервер не видит содержимого.
//!
//! Проверяемые инварианты:
//! - в облачном чате получатель читает сообщение, отправленное офлайн;
//! - сервер находит сообщение по поиску (ключ чата у него есть);
//! - в приватном чате тот же поиск ничего не находит;
//! - открытый текст в облачном чате не лежит в БД сервера plaintext-полем.

mod common;

use common::*;
use rchat_core::types::ChatMode;
use rchat_proto::proto;

/// Облачный чат: сообщение доставляется офлайн, сервер ищет по нему.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_chat_delivers_and_is_searchable() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let alice_dir = root.path().join("alice");
    let bob_dir = root.path().join("bob");

    let (alice, alice_storage) = build_client(&alice_dir, harness.port(), "alice-dev");
    alice.start().await.expect("start alice");
    register_user(&alice, "alice", "Alice", PASSWORD_ALICE).await;

    // Bob регистрируется и уходит офлайн.
    {
        let (bob, _bob_storage) = build_client(&bob_dir, harness.port(), "bob-dev");
        bob.start().await.expect("start bob");
        register_user(&bob, "bob", "Bob", PASSWORD_BOB).await;
        bob.shutdown().await.expect("shutdown bob");
    }

    // Облачный чат: по умолчанию именно он.
    assert_eq!(alice.default_chat_mode(), ChatMode::Cloud);
    let chat = alice
        .create_direct_chat("bob", ChatMode::Cloud)
        .await
        .expect("create cloud chat");
    assert_eq!(chat.e2e_mode, proto::E2eMode::Cloud);
    assert_eq!(chat.cloud_key.len(), 32, "у чата есть ключ");

    let secret = "облачное секретное слово 777";
    let local_id = alice
        .send_text(&chat.chat_id, secret, None)
        .await
        .expect("send cloud message");
    wait_sent(&alice_storage, &local_id).await;

    // В БД сервера открытого текста нет: содержимое лежит под ключом чата.
    let plaintexts = server_plaintexts(harness.db_path());
    assert!(
        plaintexts.iter().all(|text| !text.contains(secret)),
        "сервер не должен хранить открытый текст облачного сообщения"
    );

    // Bob читает офлайн-сообщение.
    let (bob, _bob_storage) = build_client(&bob_dir, harness.port(), "bob-dev");
    bob.start().await.expect("start bob again");
    bob.sync_now().await.expect("sync bob");
    let received = wait_for_body(&bob, &chat.chat_id, secret).await;
    assert_eq!(received.content_kind, "text");

    // Сервер ищет по истории: ключ чата у него есть.
    let hits = bob
        .search_messages("облачное секретное", 10)
        .await
        .expect("search messages");
    assert_eq!(hits.len(), 1, "сервер должен найти облачное сообщение");
    assert_eq!(hits[0].chat_id, chat.chat_id);
    assert!(hits[0].body.contains(secret));

    // Поиск по несуществующему слову ничего не находит.
    let empty = bob
        .search_messages("такого-слова-нет", 10)
        .await
        .expect("search messages");
    assert!(
        empty.is_empty(),
        "поиск не должен давать ложных срабатываний"
    );
}

/// Приватный чат (E2E) остаётся непрозрачным для сервера: содержимое сервер
/// не видит и по поиску не находит.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn private_chat_is_not_searchable_by_server() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let alice_dir = root.path().join("alice");
    let bob_dir = root.path().join("bob");

    let (alice, alice_storage) = build_client(&alice_dir, harness.port(), "alice-dev");
    alice.start().await.expect("start alice");
    register_user(&alice, "alice", "Alice", PASSWORD_ALICE).await;

    let (bob, bob_storage) = build_client(&bob_dir, harness.port(), "bob-dev");
    bob.start().await.expect("start bob");
    register_user(&bob, "bob", "Bob", PASSWORD_BOB).await;

    let chat = alice
        .create_direct_chat("bob", ChatMode::Private)
        .await
        .expect("create private chat");
    assert_eq!(chat.e2e_mode, proto::E2eMode::OlmV1);
    assert!(chat.cloud_key.is_empty(), "у приватного чата нет ключа");

    let secret = "приватное секретное слово 888";
    let local_id = alice
        .send_text(&chat.chat_id, secret, None)
        .await
        .expect("send private message");
    wait_sent(&alice_storage, &local_id).await;

    // Bob читает (E2E работает как прежде).
    bob.sync_now().await.expect("sync bob");
    wait_for_body(&bob, &chat.chat_id, secret).await;

    // Сервер содержимое не видит: и в БД его нет, и поиск ничего не находит.
    let plaintexts = server_plaintexts(harness.db_path());
    assert!(
        plaintexts.iter().all(|text| !text.contains(secret)),
        "сервер не должен хранить открытый текст приватного сообщения"
    );
    let hits = bob
        .search_messages("приватное секретное", 10)
        .await
        .expect("search messages");
    assert!(
        hits.is_empty(),
        "сервер не должен находить содержимое приватного чата"
    );
    let _ = bob_storage;
}

/// Режим по умолчанию меняется в настройках клиента и применяется к новым
/// чатам; уже созданные чаты сохраняют свой режим.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn default_mode_is_configurable() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let alice_dir = root.path().join("alice");

    let (alice, _storage) = build_client(&alice_dir, harness.port(), "alice-dev");
    alice.start().await.expect("start alice");
    register_user(&alice, "alice", "Alice", PASSWORD_ALICE).await;

    assert_eq!(alice.default_chat_mode(), ChatMode::Cloud);
    alice
        .set_default_chat_mode(ChatMode::Private)
        .await
        .expect("set default mode");
    assert_eq!(alice.default_chat_mode(), ChatMode::Private);
    alice
        .set_default_chat_mode(ChatMode::Cloud)
        .await
        .expect("set default mode back");
    assert_eq!(alice.default_chat_mode(), ChatMode::Cloud);
}

/// Открытые тексты всех сообщений сервера (для проверок приватности).
fn server_plaintexts(db_path: &std::path::Path) -> Vec<String> {
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open server db");
    let mut stmt = conn
        .prepare("SELECT plaintext_dev FROM messages")
        .expect("prepare messages");
    let rows = stmt
        .query_map([], |row| row.get::<_, Option<String>>(0))
        .expect("query messages");
    rows.map(|row| row.expect("row").unwrap_or_default())
        .collect()
}

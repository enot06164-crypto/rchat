//! Сквозные сценарии групповых чатов с E2E (Megolm): реальный сервер,
//! реальные клиенты (ядро + SQLite + провайдер Olm) поверх WebSocket.
//!
//! Проверяемые инварианты:
//! - сервер хранит только общий megolm-ciphertext и зашифрованные ключи;
//! - ключ сессии выдаётся каждому устройству участника один раз (повторная
//!   отправка не дублирует ключ);
//! - приглашённый участник получает ключ только следующих сообщений: более
//!   ранние ему недоступны;
//! - исключение участника ротирует исходящую сессию: исключённый не читает
//!   новые сообщения.

mod common;

use std::path::Path;

use common::*;
use rchat_core::client::Client;
use rchat_core::error::CoreError;
use rchat_core::traits::Storage;
use rchat_core::types::MessageRecord;
use rchat_proto::proto;
use rusqlite::Connection;

/// Ждёт появления чата в локальном списке клиента.
async fn wait_for_chat(client: &Client, chat_id: &str) {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let chats = client.list_chats().await.expect("list chats");
        if chats.iter().any(|chat| chat.chat_id == chat_id) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "не дождались чата {chat_id}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }
}

/// Ждёт, пока в чате появится сообщение с указанным `server_id`.
async fn wait_for_message(client: &Client, chat_id: &str, server_id: u64) -> MessageRecord {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let messages = client
            .list_messages(chat_id, None, 100)
            .await
            .expect("list messages");
        if let Some(message) = messages
            .into_iter()
            .find(|message| message.server_id == Some(server_id))
        {
            return message;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "не дождались сообщения #{server_id}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }
}

/// `(group_ciphertext, group_session_id)` сообщения из серверной БД.
fn server_group_payload(db_path: &Path, server_id: u64) -> (Vec<u8>, String) {
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open server db");
    conn.query_row(
        "SELECT group_ciphertext, group_session_id FROM messages WHERE server_id = ?1",
        [server_id as i64],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .expect("select group payload")
}

/// Создание E2E-группы, обмен сообщениями, приглашение участника, исключение
/// с ротацией сессии — полный жизненный цикл группового чата.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn group_e2e_lifecycle_with_rotation() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");

    let (alice, alice_storage) =
        build_client(&root.path().join("alice"), harness.port(), "alice-dev");
    alice.start().await.expect("start alice");
    register_user(&alice, "alice", "Alice", PASSWORD_ALICE).await;

    let (bob, bob_storage) = build_client(&root.path().join("bob"), harness.port(), "bob-dev");
    bob.start().await.expect("start bob");
    register_user(&bob, "bob", "Bob", PASSWORD_BOB).await;

    let (carol, _carol_storage) =
        build_client(&root.path().join("carol"), harness.port(), "carol-dev");
    carol.start().await.expect("start carol");
    register_user(&carol, "carol", "Carol", "pw-carol-1").await;

    let (carol_user_id, _, _) = carol.session().expect("carol session");

    // Группа с двумя участниками; создатель — владелец.
    let chat = alice
        .create_group_chat("Проект", &["bob".to_string(), "carol".to_string()], true)
        .await
        .expect("create group");
    assert_eq!(chat.kind, proto::ChatKind::Group);
    assert_eq!(chat.e2e_mode, proto::E2eMode::OlmV1);
    assert_eq!(chat.title, "Проект");
    let members = alice.chat_members(&chat.chat_id).await.expect("members");
    assert_eq!(members.len(), 3);
    let owner = members
        .iter()
        .find(|member| member.role == proto::ChatMemberRole::Owner)
        .expect("owner exists");
    assert_eq!(owner.username, "alice");

    wait_for_chat(&bob, &chat.chat_id).await;
    wait_for_chat(&carol, &chat.chat_id).await;

    // Первое сообщение: сервер видит только megolm-ciphertext и два
    // зашифрованных ключа (Bob и Carol).
    let first = "первое групповое сообщение";
    let first_id = alice
        .send_text(&chat.chat_id, first, None)
        .await
        .expect("send first");
    wait_sent(&alice_storage, &first_id).await;
    let first_server_id = server_id_of(&alice_storage, &first_id).await;
    let (messages, recipients) = assert_server_has_no_plaintext(harness.db_path(), first);
    assert_eq!(messages, 1);
    assert_eq!(recipients, 2, "ключ уходит обоим участникам");
    let (group_ciphertext, first_session_id) =
        server_group_payload(harness.db_path(), first_server_id);
    assert!(!group_ciphertext.is_empty(), "megolm-ciphertext пуст");
    assert!(!first_session_id.is_empty(), "session_id пуст");

    bob.sync_now().await.expect("sync bob");
    wait_for_body(&bob, &chat.chat_id, first).await;
    carol.sync_now().await.expect("sync carol");
    wait_for_body(&carol, &chat.chat_id, first).await;

    // Повторная отправка тем же отправителем: ключ уже выдан — recipients пусты.
    let second = "второе сообщение";
    let second_id = alice
        .send_text(&chat.chat_id, second, None)
        .await
        .expect("send second");
    wait_sent(&alice_storage, &second_id).await;
    let second_server_id = server_id_of(&alice_storage, &second_id).await;
    assert_eq!(
        message_recipient_count(harness.db_path(), second_server_id),
        0,
        "ключ не должен дублироваться"
    );

    bob.sync_now().await.expect("sync bob");
    wait_for_body(&bob, &chat.chat_id, second).await;

    // Ответ Bob: своя сессия, ключ уходит Alice и Carol.
    let reply = "ответ боба";
    let reply_id = bob
        .send_text(&chat.chat_id, reply, None)
        .await
        .expect("send reply");
    let reply_server_id = {
        wait_sent(&bob_storage, &reply_id).await;
        server_id_of(&bob_storage, &reply_id).await
    };
    assert_eq!(
        message_recipient_count(harness.db_path(), reply_server_id),
        2,
        "ответ уходит Alice и Carol"
    );
    alice.sync_now().await.expect("sync alice");
    wait_for_body(&alice, &chat.chat_id, reply).await;
    carol.sync_now().await.expect("sync carol");
    wait_for_body(&carol, &chat.chat_id, reply).await;

    // Исключение Carol: исходящая сессия Alice ротируется.
    alice
        .remove_chat_member(&chat.chat_id, &carol_user_id)
        .await
        .expect("remove carol");
    let members = alice.chat_members(&chat.chat_id).await.expect("members");
    assert_eq!(members.len(), 2, "состав без Carol");

    let third = "сообщение после исключения";
    let third_id = alice
        .send_text(&chat.chat_id, third, None)
        .await
        .expect("send third");
    wait_sent(&alice_storage, &third_id).await;
    let third_server_id = server_id_of(&alice_storage, &third_id).await;
    let (_, third_session_id) = server_group_payload(harness.db_path(), third_server_id);
    assert_ne!(
        third_session_id, first_session_id,
        "после исключения должна использоваться новая сессия"
    );
    assert_eq!(
        message_recipient_count(harness.db_path(), third_server_id),
        1,
        "ключ уходит только Bob"
    );

    // Bob читает; Carol не получает сообщение вовсе: она исключена из чата
    // (событие журнала), а ключ новой сессии ей не выдавался.
    bob.sync_now().await.expect("sync bob");
    wait_for_body(&bob, &chat.chat_id, third).await;
    carol.sync_now().await.expect("sync carol");
    let carol_members = carol.chat_members(&chat.chat_id).await.expect("members");
    assert_eq!(carol_members.len(), 2, "Carol видит состав без себя");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let carol_messages = carol
        .list_messages(&chat.chat_id, None, 100)
        .await
        .expect("carol messages");
    assert!(
        !carol_messages
            .iter()
            .any(|message| message.server_id == Some(third_server_id)),
        "исключённая участница не должна получать новые сообщения"
    );

    carol.shutdown().await.expect("shutdown carol");
    bob.shutdown().await.expect("shutdown bob");
    alice.shutdown().await.expect("shutdown alice");
    harness.shutdown().await;
}

/// Приглашённый участник получает ключ только с момента приглашения: более
/// ранние сообщения остаются недоступными.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn group_late_member_cannot_read_earlier_messages() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");

    let (alice, alice_storage) =
        build_client(&root.path().join("alice"), harness.port(), "alice-dev");
    alice.start().await.expect("start alice");
    register_user(&alice, "alice", "Alice", PASSWORD_ALICE).await;

    let (bob, _bob_storage) = build_client(&root.path().join("bob"), harness.port(), "bob-dev");
    bob.start().await.expect("start bob");
    register_user(&bob, "bob", "Bob", PASSWORD_BOB).await;

    let (carol, _carol_storage) =
        build_client(&root.path().join("carol"), harness.port(), "carol-dev");
    carol.start().await.expect("start carol");
    register_user(&carol, "carol", "Carol", "pw-carol-1").await;

    let chat = alice
        .create_group_chat("Кружок", &["bob".to_string()], true)
        .await
        .expect("create group");

    let early = "раннее сообщение";
    let early_id = alice
        .send_text(&chat.chat_id, early, None)
        .await
        .expect("send early");
    wait_sent(&alice_storage, &early_id).await;
    let early_server_id = server_id_of(&alice_storage, &early_id).await;

    // Carol приглашается после первого сообщения.
    alice
        .invite_to_chat(&chat.chat_id, &["carol".to_string()])
        .await
        .expect("invite carol");
    wait_for_chat(&carol, &chat.chat_id).await;

    let late = "сообщение для всех";
    let late_id = alice
        .send_text(&chat.chat_id, late, None)
        .await
        .expect("send late");
    wait_sent(&alice_storage, &late_id).await;
    let late_server_id = server_id_of(&alice_storage, &late_id).await;
    assert_eq!(
        message_recipient_count(harness.db_path(), late_server_id),
        1,
        "ключ должен уйти только новому участнику"
    );

    carol.sync_now().await.expect("sync carol");
    let late_for_carol = wait_for_body(&carol, &chat.chat_id, late).await;
    assert_eq!(late_for_carol.server_id, Some(late_server_id));
    // Раннее сообщение доступно только через историю: текст недостижим —
    // ключа той сессии у Carol нет.
    carol
        .fetch_history(&chat.chat_id, None, 50)
        .await
        .expect("fetch history");
    let early_for_carol = wait_for_message(&carol, &chat.chat_id, early_server_id).await;
    assert!(
        early_for_carol.body.is_none(),
        "приглашённый позже не должен читать ранние сообщения"
    );
    assert_eq!(
        message_recipient_count(harness.db_path(), early_server_id),
        1,
        "раннее сообщение адресовано только Bob"
    );

    // Bob читает оба сообщения: ключ был выдан раньше и ратчет продолжается.
    bob.sync_now().await.expect("sync bob");
    wait_for_body(&bob, &chat.chat_id, early).await;
    wait_for_body(&bob, &chat.chat_id, late).await;

    carol.shutdown().await.expect("shutdown carol");
    bob.shutdown().await.expect("shutdown bob");
    alice.shutdown().await.expect("shutdown alice");
    harness.shutdown().await;
}

/// Мультиустройство в группе: сообщение зашифровано каждому устройству
/// участника, включая второе устройство отправителя.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn group_e2e_multi_device() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");

    let (alice1, _alice1_storage) =
        build_client(&root.path().join("alice1"), harness.port(), "alice-dev1");
    alice1.start().await.expect("start alice1");
    register_user(&alice1, "alice", "Alice", PASSWORD_ALICE).await;

    let (alice2, alice2_storage) =
        build_client(&root.path().join("alice2"), harness.port(), "alice-dev2");
    alice2.start().await.expect("start alice2");
    login_user(&alice2, "alice", PASSWORD_ALICE).await;

    let (bob, bob_storage) = build_client(&root.path().join("bob"), harness.port(), "bob-dev");
    bob.start().await.expect("start bob");
    register_user(&bob, "bob", "Bob", PASSWORD_BOB).await;

    let chat = alice1
        .create_group_chat("Двое на связи", &["bob".to_string()], true)
        .await
        .expect("create group");
    wait_for_chat(&alice2, &chat.chat_id).await;
    wait_for_chat(&bob, &chat.chat_id).await;

    // Bob пишет в группу: ключ уходит обоим устройствам Alice.
    let from_bob = "привет группе";
    let bob_id = bob
        .send_text(&chat.chat_id, from_bob, None)
        .await
        .expect("send from bob");
    wait_sent(&bob_storage, &bob_id).await;
    assert_eq!(
        message_recipient_count(harness.db_path(), server_id_of(&bob_storage, &bob_id).await),
        2,
        "ключ уходит двум устройствам Alice"
    );
    alice1.sync_now().await.expect("sync alice1");
    wait_for_body(&alice1, &chat.chat_id, from_bob).await;
    alice2.sync_now().await.expect("sync alice2");
    wait_for_body(&alice2, &chat.chat_id, from_bob).await;

    // Alice отвечает со второго устройства: копию получают Bob и первое
    // устройство Alice (своё второе устройство не дублируется).
    let from_alice2 = "отвечаю со второго устройства";
    let alice2_id = alice2
        .send_text(&chat.chat_id, from_alice2, None)
        .await
        .expect("send from alice2");
    wait_sent(&alice2_storage, &alice2_id).await;
    assert_eq!(
        message_recipient_count(
            harness.db_path(),
            server_id_of(&alice2_storage, &alice2_id).await
        ),
        2,
        "ключ уходит Bob и первому устройству Alice"
    );
    bob.sync_now().await.expect("sync bob");
    wait_for_body(&bob, &chat.chat_id, from_alice2).await;
    alice1.sync_now().await.expect("sync alice1");
    wait_for_body(&alice1, &chat.chat_id, from_alice2).await;
    assert_server_has_no_plaintext(harness.db_path(), from_alice2);

    // Следующее сообщение уже не несёт ключей: у всех есть сессия.
    let again = "ещё одно";
    let again_id = alice2
        .send_text(&chat.chat_id, again, None)
        .await
        .expect("send again");
    wait_sent(&alice2_storage, &again_id).await;
    assert_eq!(
        message_recipient_count(
            harness.db_path(),
            server_id_of(&alice2_storage, &again_id).await
        ),
        0,
        "ключ уже выдан всем устройствам"
    );

    bob.shutdown().await.expect("shutdown bob");
    alice2.shutdown().await.expect("shutdown alice2");
    alice1.shutdown().await.expect("shutdown alice1");
    harness.shutdown().await;
}

/// `server_id` подтверждённого собственного сообщения.
async fn server_id_of(
    storage: &std::sync::Arc<rchat_client_storage_sqlite::ClientSqliteStorage>,
    local_id: &str,
) -> u64 {
    storage
        .get_message(local_id)
        .await
        .expect("get message")
        .expect("message exists")
        .server_id
        .expect("server id")
}

/// Dev-группы (без E2E) запрещены, пока сервер не разрешил dev-plaintext:
/// клиент не должен молча деградировать до открытого текста.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dev_group_rejected_by_default() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");

    let (alice, _alice_storage) =
        build_client(&root.path().join("alice"), harness.port(), "alice-dev");
    alice.start().await.expect("start alice");
    register_user(&alice, "alice", "Alice", PASSWORD_ALICE).await;

    let (bob, _bob_storage) = build_client(&root.path().join("bob"), harness.port(), "bob-dev");
    bob.start().await.expect("start bob");
    register_user(&bob, "bob", "Bob", PASSWORD_BOB).await;

    let error = alice
        .create_group_chat("Черновик", &["bob".to_string()], false)
        .await
        .expect_err("dev group must be rejected by default");
    match error {
        CoreError::Rejected { .. } => {}
        other => panic!("неожиданная ошибка: {other}"),
    }

    bob.shutdown().await.expect("shutdown bob");
    alice.shutdown().await.expect("shutdown alice");
    harness.shutdown().await;
}

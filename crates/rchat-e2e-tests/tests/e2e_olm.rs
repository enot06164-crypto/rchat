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
//!
//! Общий харнесс — в `tests/common/mod.rs`.

mod common;

use common::*;
use rchat_core::traits::Storage;
use rchat_proto::proto;

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

//! Несколько независимых аккаунтов в одном процессе: у каждого клиента своё
//! хранилище, секреты и E2E-состояние, а фоновые акторы работают параллельно
//! в общем tokio-runtime.
//!
//! Проверяемые инварианты:
//! - три клиента (alice, bob, carol) живут в одном процессе и одновременно
//!   отправляют/получают сообщения;
//! - чаты одного аккаунта не появляются в состоянии другого;
//! - один аккаунт может состоять в двух разных личных E2E-чатах и читать оба.

mod common;

use common::*;
use rchat_core::traits::Storage;
use rchat_core::types::ChatMode;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accounts_in_one_process_are_isolated() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");

    let (alice, alice_storage) =
        build_client(&root.path().join("alice"), harness.port(), "alice-dev");
    let (bob, bob_storage) = build_client(&root.path().join("bob"), harness.port(), "bob-dev");
    let (carol, carol_storage) =
        build_client(&root.path().join("carol"), harness.port(), "carol-dev");
    for (client, username, display_name, password) in [
        (&alice, "alice", "Alice", PASSWORD_ALICE),
        (&bob, "bob", "Bob", PASSWORD_BOB),
        (&carol, "carol", "Carol", "pw-carol-1"),
    ] {
        client.start().await.expect("start client");
        register_user(client, username, display_name, password).await;
    }

    // У аккаунтов разные личности, хотя все три клиента в одном процессе.
    let (alice_user_id, _, _) = alice.session().expect("alice session");
    let (bob_user_id, _, _) = bob.session().expect("bob session");
    let (carol_user_id, _, _) = carol.session().expect("carol session");
    assert_ne!(alice_user_id, bob_user_id);
    assert_ne!(bob_user_id, carol_user_id);
    let alice_account = alice_storage
        .load_account()
        .await
        .expect("account")
        .expect("alice profile");
    let bob_account = bob_storage
        .load_account()
        .await
        .expect("account")
        .expect("bob profile");
    let carol_account = carol_storage
        .load_account()
        .await
        .expect("account")
        .expect("carol profile");
    assert_eq!(alice_account.username, "alice");
    assert_eq!(bob_account.username, "bob");
    assert_eq!(carol_account.username, "carol");
    assert_ne!(alice_account.device_id, bob_account.device_id);
    assert_ne!(bob_account.device_id, carol_account.device_id);

    // Два независимых E2E-чата: alice↔bob и bob↔carol.
    let ab = alice
        .create_direct_chat("bob", ChatMode::Private)
        .await
        .expect("chat alice-bob");
    let bc = bob
        .create_direct_chat("carol", ChatMode::Private)
        .await
        .expect("chat bob-carol");

    // Одновременная отправка из двух клиентов одного процесса.
    let (alice_local, carol_local) = tokio::join!(
        alice.send_text(&ab.chat_id, "от alice 314", None),
        carol.send_text(&bc.chat_id, "от carol 271", None),
    );
    wait_sent(&alice_storage, &alice_local.expect("send alice")).await;
    wait_sent(&carol_storage, &carol_local.expect("send carol")).await;

    // Bob — участник обоих чатов и расшифровывает оба сообщения.
    bob.sync_now().await.expect("sync bob");
    let from_alice = wait_for_body(&bob, &ab.chat_id, "от alice 314").await;
    let from_carol = wait_for_body(&bob, &bc.chat_id, "от carol 271").await;
    assert_eq!(from_alice.sender_user_id, alice_user_id);
    assert_eq!(from_carol.sender_user_id, carol_user_id);

    // Состояние чатов не протекает между аккаунтами.
    alice.sync_now().await.expect("sync alice");
    carol.sync_now().await.expect("sync carol");
    let alice_chats = alice.list_chats().await.expect("alice chats");
    let carol_chats = carol.list_chats().await.expect("carol chats");
    assert!(
        alice_chats.iter().all(|chat| chat.chat_id != bc.chat_id),
        "alice не должна видеть чат bob↔carol"
    );
    assert!(
        carol_chats.iter().all(|chat| chat.chat_id != ab.chat_id),
        "carol не должна видеть чат alice↔bob"
    );
    assert!(alice_chats.iter().any(|chat| chat.chat_id == ab.chat_id));
    assert!(carol_chats.iter().any(|chat| chat.chat_id == bc.chat_id));
    assert!(bob_storage
        .get_chat(&ab.chat_id)
        .await
        .expect("chat")
        .is_some());
    assert!(bob_storage
        .get_chat(&bc.chat_id)
        .await
        .expect("chat")
        .is_some());

    carol.shutdown().await.expect("shutdown carol");
    bob.shutdown().await.expect("shutdown bob");
    alice.shutdown().await.expect("shutdown alice");
    harness.shutdown().await;
}

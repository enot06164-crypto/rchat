//! Сквозной тест запроса на приватный режим (E2E) в личном чате.
//!
//! Сценарий: облачный чат → запрос от первого участника → второй участник
//! получает предложение и принимает → чат переходит в приватный режим, и
//! сообщения в нём шифруются end-to-end.

mod common;

use common::*;
use rchat_core::events::{EventSubscription, UiEvent};
use rchat_core::types::ChatMode;
use rchat_proto::proto;

/// Ждёт события о приватном режиме: `requested` — предложение пришло,
/// `resolved` — запрос разрешён или отклонён.
async fn wait_private_mode_event(
    subscription: &mut EventSubscription,
    kind: PrivateModeEvent,
) -> bool {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, subscription.next()).await {
            Ok(Ok(UiEvent::PrivateModeRequested { .. })) if kind == PrivateModeEvent::Requested => {
                return true
            }
            Ok(Ok(UiEvent::PrivateModeResolved { .. })) if kind == PrivateModeEvent::Resolved => {
                return true
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => continue,
            Err(_) => return false,
        }
    }
    false
}

/// Какое событие приватного режима ждём.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrivateModeEvent {
    Requested,
    Resolved,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn private_mode_request_switches_chat_to_e2e() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let alice_dir = root.path().join("alice");
    let bob_dir = root.path().join("bob");

    let (alice, alice_storage) = build_client(&alice_dir, harness.port(), "alice-dev");
    alice.start().await.expect("start alice");
    register_user(&alice, "alice", "Alice", PASSWORD_ALICE).await;

    let (bob, _bob_storage) = build_client(&bob_dir, harness.port(), "bob-dev");
    bob.start().await.expect("start bob");
    register_user(&bob, "bob", "Bob", PASSWORD_BOB).await;

    // Чат создаётся в облачном режиме.
    let chat = alice
        .create_direct_chat("bob", ChatMode::Cloud)
        .await
        .expect("create chat");
    assert_eq!(chat.e2e_mode, proto::E2eMode::Cloud);

    let mut alice_events = alice.subscribe();
    let mut bob_events = bob.subscribe();

    // Алиса просит перевести чат в приватный режим.
    let pending = alice
        .request_private_mode(&chat.chat_id)
        .await
        .expect("request private mode");
    assert!(pending, "запрос должен уйти в ожидание");

    // Боб получает предложение.
    assert!(
        wait_private_mode_event(&mut bob_events, PrivateModeEvent::Requested).await,
        "собеседник должен получить предложение приватного режима"
    );

    // Боб принимает — чат переводится в E2E.
    let switched = bob
        .resolve_private_mode(&chat.chat_id, true)
        .await
        .expect("resolve private mode");
    assert!(switched, "чат должен перейти в приватный режим");

    // Алиса узнаёт, что запрос принят.
    assert!(
        wait_private_mode_event(&mut alice_events, PrivateModeEvent::Resolved).await,
        "запросившему должно прийти разрешение"
    );

    // Чат у обоих теперь приватный.
    bob.sync_now().await.expect("sync bob");
    let bob_chats = bob.list_chats().await.expect("list chats");
    let updated = bob_chats
        .iter()
        .find(|c| c.chat_id == chat.chat_id)
        .expect("chat");
    assert_eq!(updated.e2e_mode, proto::E2eMode::OlmV1);

    // Сообщение в переведённом чате идёт end-to-end: второй участник его
    // прочитает, а в БД сервера открытого текста не будет.
    let secret = "секрет после перехода в приватный режим";
    let local_id = alice
        .send_text(&chat.chat_id, secret, None)
        .await
        .expect("send message");
    wait_sent(&alice_storage, &local_id).await;

    let received = wait_for_body(&bob, &chat.chat_id, secret).await;
    assert_eq!(received.body.as_deref(), Some(secret));

    let (messages, recipients) = assert_server_has_no_plaintext(harness.db_path(), secret);
    assert_eq!(messages, 1, "на сервере одно сообщение");
    assert_eq!(recipients, 1, "шифротекст — для устройства Bob");
}

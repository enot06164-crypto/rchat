//! Сквозные сценарии сигнализации звонков: реальный сервер, реальные клиенты
//! (ядро + SQLite + Olm) поверх WebSocket.
//!
//! Проверяемые инварианты:
//! - приглашение/принятие/завершение и отклонение доходят до обоих участников
//!   transient-событиями;
//! - звонок доступен только в личном чате, медиа нет;
//! - второй звонок при занятых участниках отклоняется;
//! - события получают все активные устройства участника (мультиустройство).

mod common;

use common::*;
use rchat_core::events::{EventSubscription, UiEvent};
use rchat_core::types::{CallRecord, CallState};

/// Ждёт следующее событие звонка, пропуская прочие UI-события.
async fn wait_call_event(subscription: &mut EventSubscription) -> UiEvent {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let event = tokio::time::timeout_at(deadline, subscription.next())
            .await
            .expect("call event timeout")
            .expect("event stream");
        if matches!(
            event,
            UiEvent::CallInvited(_)
                | UiEvent::CallAccepted(_)
                | UiEvent::CallDeclined(_)
                | UiEvent::CallEnded(_)
        ) {
            return event;
        }
    }
}

fn expect_invited(event: UiEvent) -> CallRecord {
    match event {
        UiEvent::CallInvited(call) => call,
        other => panic!("expected CallInvited, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn call_invite_accept_decline_end() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let (alice, _alice_storage) =
        build_client(&root.path().join("alice"), harness.port(), "alice-dev");
    let (bob, _bob_storage) = build_client(&root.path().join("bob"), harness.port(), "bob-dev");
    alice.start().await.expect("start alice");
    bob.start().await.expect("start bob");
    register_user(&alice, "alice", "Alice", PASSWORD_ALICE).await;
    register_user(&bob, "bob", "Bob", PASSWORD_BOB).await;

    let chat = alice
        .create_direct_chat("bob", true)
        .await
        .expect("direct chat");
    let mut alice_events = alice.subscribe();
    let mut bob_events = bob.subscribe();
    let (alice_id, _, _) = alice.session().expect("alice session");
    let (bob_id, _, _) = bob.session().expect("bob session");

    // Приглашение.
    let ringing = alice.call_invite(&chat.chat_id).await.expect("invite");
    assert_eq!(ringing.chat_id, chat.chat_id);
    assert_eq!(ringing.caller_user_id, alice_id);
    assert_eq!(ringing.callee_user_id, bob_id);
    assert_eq!(ringing.state, CallState::Ringing);
    let invited = expect_invited(wait_call_event(&mut bob_events).await);
    assert_eq!(invited.call_id, ringing.call_id);

    // Принять может только callee.
    assert!(alice.call_accept(&ringing.call_id).await.is_err());
    let accepted = bob.call_accept(&ringing.call_id).await.expect("accept");
    assert_eq!(accepted.state, CallState::Active);
    assert!(accepted.accepted_at_ms > 0);
    match wait_call_event(&mut alice_events).await {
        UiEvent::CallAccepted(call) => assert_eq!(call.call_id, ringing.call_id),
        other => panic!("expected CallAccepted, got {other:?}"),
    }
    match wait_call_event(&mut bob_events).await {
        UiEvent::CallAccepted(call) => assert_eq!(call.state, CallState::Active),
        other => panic!("expected CallAccepted, got {other:?}"),
    }

    // Завершает любой участник.
    let ended = bob.call_end(&ringing.call_id).await.expect("end");
    assert_eq!(ended.state, CallState::Active);
    match wait_call_event(&mut alice_events).await {
        UiEvent::CallEnded(call) => assert_eq!(call.call_id, ringing.call_id),
        other => panic!("expected CallEnded, got {other:?}"),
    }
    match wait_call_event(&mut bob_events).await {
        UiEvent::CallEnded(call) => assert_eq!(call.call_id, ringing.call_id),
        other => panic!("expected CallEnded, got {other:?}"),
    }

    // Отклонение ringing-звонка.
    let ringing = alice.call_invite(&chat.chat_id).await.expect("invite 2");
    let second = expect_invited(wait_call_event(&mut bob_events).await);
    assert_eq!(second.call_id, ringing.call_id);
    let declined = bob.call_decline(&ringing.call_id).await.expect("decline");
    assert_eq!(declined.state, CallState::Ringing);
    match wait_call_event(&mut alice_events).await {
        UiEvent::CallDeclined(call) => assert_eq!(call.call_id, ringing.call_id),
        other => panic!("expected CallDeclined, got {other:?}"),
    }
    match wait_call_event(&mut bob_events).await {
        UiEvent::CallDeclined(call) => assert_eq!(call.call_id, ringing.call_id),
        other => panic!("expected CallDeclined, got {other:?}"),
    }

    // Пока звонок идёт, второй невозможен; после завершения — снова можно.
    let ringing = alice.call_invite(&chat.chat_id).await.expect("invite 3");
    assert!(alice.call_invite(&chat.chat_id).await.is_err(), "busy");
    let ended = alice.call_end(&ringing.call_id).await.expect("end 3");
    assert_eq!(ended.state, CallState::Ringing);
    let _ = expect_invited(wait_call_event(&mut bob_events).await);
    match wait_call_event(&mut bob_events).await {
        UiEvent::CallEnded(call) => assert_eq!(call.call_id, ringing.call_id),
        other => panic!("expected CallEnded, got {other:?}"),
    }

    bob.shutdown().await.expect("shutdown bob");
    alice.shutdown().await.expect("shutdown alice");
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn call_events_reach_all_devices() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let (alice, _alice_storage) =
        build_client(&root.path().join("alice"), harness.port(), "alice-dev");
    let (bob1, _bob1_storage) = build_client(&root.path().join("bob1"), harness.port(), "bob-dev1");
    let (bob2, _bob2_storage) = build_client(&root.path().join("bob2"), harness.port(), "bob-dev2");
    alice.start().await.expect("start alice");
    bob1.start().await.expect("start bob1");
    bob2.start().await.expect("start bob2");
    register_user(&alice, "alice", "Alice", PASSWORD_ALICE).await;
    register_user(&bob1, "bob", "Bob", PASSWORD_BOB).await;
    login_user(&bob2, "bob", PASSWORD_BOB).await;

    let chat = alice
        .create_direct_chat("bob", true)
        .await
        .expect("direct chat");
    let mut alice_events = alice.subscribe();
    let mut bob1_events = bob1.subscribe();
    let mut bob2_events = bob2.subscribe();

    let ringing = alice.call_invite(&chat.chat_id).await.expect("invite");
    // Оба устройства Bob звонят.
    let invited1 = expect_invited(wait_call_event(&mut bob1_events).await);
    let invited2 = expect_invited(wait_call_event(&mut bob2_events).await);
    assert_eq!(invited1.call_id, ringing.call_id);
    assert_eq!(invited2.call_id, ringing.call_id);

    // Принятие на первом устройстве гасит звонок на втором.
    let accepted = bob1.call_accept(&ringing.call_id).await.expect("accept");
    assert_eq!(accepted.state, CallState::Active);
    match wait_call_event(&mut bob2_events).await {
        UiEvent::CallAccepted(call) => assert_eq!(call.call_id, ringing.call_id),
        other => panic!("expected CallAccepted, got {other:?}"),
    }
    match wait_call_event(&mut alice_events).await {
        UiEvent::CallAccepted(call) => assert_eq!(call.call_id, ringing.call_id),
        other => panic!("expected CallAccepted, got {other:?}"),
    }

    // Завершение с первого устройства видят все.
    bob1.call_end(&ringing.call_id).await.expect("end");
    match wait_call_event(&mut bob2_events).await {
        UiEvent::CallEnded(call) => assert_eq!(call.call_id, ringing.call_id),
        other => panic!("expected CallEnded, got {other:?}"),
    }
    match wait_call_event(&mut alice_events).await {
        UiEvent::CallEnded(call) => assert_eq!(call.call_id, ringing.call_id),
        other => panic!("expected CallEnded, got {other:?}"),
    }

    bob2.shutdown().await.expect("shutdown bob2");
    bob1.shutdown().await.expect("shutdown bob1");
    alice.shutdown().await.expect("shutdown alice");
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn calls_are_rejected_in_group_chat() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let (alice, _alice_storage) =
        build_client(&root.path().join("alice"), harness.port(), "alice-dev");
    let (bob, _bob_storage) = build_client(&root.path().join("bob"), harness.port(), "bob-dev");
    alice.start().await.expect("start alice");
    bob.start().await.expect("start bob");
    register_user(&alice, "alice", "Alice", PASSWORD_ALICE).await;
    register_user(&bob, "bob", "Bob", PASSWORD_BOB).await;

    let group = alice
        .create_group_chat("Группа", &["bob".to_string()], true)
        .await
        .expect("group");
    let error = alice
        .call_invite(&group.chat_id)
        .await
        .expect_err("no group calls");
    assert!(error.to_string().contains("direct chats"), "error: {error}");

    bob.shutdown().await.expect("shutdown bob");
    alice.shutdown().await.expect("shutdown alice");
    harness.shutdown().await;
}

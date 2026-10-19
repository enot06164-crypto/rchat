//! Интеграционные тесты сигнализации звонков на сервисном слое: только
//! личные чаты, приглашение/принятие/отклонение/завершение, busy/offline,
//! роли участников и transient push-события.

mod common;

use std::time::Duration;

use common::*;
use rchat_proto::proto;
use rchat_proto::proto::request::Method;
use rchat_proto::proto::response::Result as R;
use rchat_server::service::{ConnState, Server};
use tokio::sync::mpsc;

/// Достаёт из push-канала следующее transient-событие звонка, пропуская
/// события журнала (например, создание чата).
async fn next_call_event(rx: &mut mpsc::Receiver<Vec<u8>>) -> proto::CallEvent {
    for _ in 0..32 {
        let frame = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("call event timeout")
            .expect("push channel closed");
        let envelope: proto::Envelope =
            rchat_proto::decode_message(&frame).expect("decode push envelope");
        if let Some(proto::envelope::Body::Push(push)) = envelope.body {
            if let Some(proto::push::Event::Transient(transient)) = push.event {
                if let Some(proto::transient_event::Event::Call(event)) = transient.event {
                    return event;
                }
            }
        }
    }
    panic!("no call event in push channel");
}

fn call_event_kind(event: &proto::CallEvent) -> proto::CallEventKind {
    proto::CallEventKind::try_from(event.kind).expect("known call event kind")
}

fn expect_call(result: R) -> proto::Call {
    match result {
        R::CallInvite(response) => response.call.expect("call in invite response"),
        R::CallAccept(response) => response.call.expect("call in accept response"),
        R::CallDecline(response) => response.call.expect("call in decline response"),
        R::CallEnd(response) => response.call.expect("call in end response"),
        other => panic!("expected call response, got {other:?}"),
    }
}

async fn invite(server: &Server, state: &mut ConnState, chat_id: &str) -> proto::Call {
    expect_call(
        call(
            server,
            state,
            Method::CallInvite(proto::CallInviteRequest {
                chat_id: chat_id.to_string(),
            }),
        )
        .await,
    )
}

#[tokio::test]
async fn invite_accept_end_with_push_events() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    let (alice_id, _) = register(&server, &mut alice, "alice", "a1", false).await;
    let (bob_id, _) = register(&server, &mut bob, "bob", "b1", false).await;
    let (_alice_conn, mut alice_rx) = server.connections().register(&alice_id, "a1", 16);
    let (_bob_conn, mut bob_rx) = server.connections().register(&bob_id, "b1", 16);
    let chat = create_direct_chat(&server, &mut alice, "bob", proto::E2eMode::NoneDev).await;

    // Приглашение: callee и caller получают transient INVITED.
    let ringing = invite(&server, &mut alice, &chat.chat_id).await;
    assert_eq!(ringing.chat_id, chat.chat_id);
    assert_eq!(ringing.caller_user_id, alice_id);
    assert_eq!(ringing.callee_user_id, bob_id);
    assert_eq!(ringing.state, proto::CallState::Ringing as i32);
    assert_eq!(ringing.accepted_at_ms, 0);
    let event = next_call_event(&mut bob_rx).await;
    assert_eq!(call_event_kind(&event), proto::CallEventKind::Invited);
    assert_eq!(event.call.expect("call").call_id, ringing.call_id);

    // Принять может только callee; после принятия событие видят оба.
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::CallAccept(proto::CallAcceptRequest {
                call_id: ringing.call_id.clone(),
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::Forbidden as i32);

    let accepted = expect_call(
        call(
            &server,
            &mut bob,
            Method::CallAccept(proto::CallAcceptRequest {
                call_id: ringing.call_id.clone(),
            }),
        )
        .await,
    );
    assert_eq!(accepted.state, proto::CallState::Active as i32);
    assert!(accepted.accepted_at_ms > 0);
    let event = next_call_event(&mut alice_rx).await;
    assert_eq!(call_event_kind(&event), proto::CallEventKind::Accepted);
    assert_eq!(
        event.call.expect("call").state,
        proto::CallState::Active as i32
    );
    let event = next_call_event(&mut bob_rx).await;
    assert_eq!(call_event_kind(&event), proto::CallEventKind::Accepted);

    // Завершает любой участник; callee получает ENDED.
    let ended = expect_call(
        call(
            &server,
            &mut alice,
            Method::CallEnd(proto::CallEndRequest {
                call_id: ringing.call_id.clone(),
            }),
        )
        .await,
    );
    assert_eq!(ended.state, proto::CallState::Active as i32);
    let event = next_call_event(&mut bob_rx).await;
    assert_eq!(call_event_kind(&event), proto::CallEventKind::Ended);

    // Завершённый звонок больше не существует.
    let error = expect_error(
        call(
            &server,
            &mut bob,
            Method::CallEnd(proto::CallEndRequest {
                call_id: ringing.call_id,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::NotFound as i32);
}

#[tokio::test]
async fn decline_busy_offline_and_roles() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    let mut carol = ConnState::new(peer());
    let (alice_id, _) = register(&server, &mut alice, "alice", "a1", false).await;
    let (bob_id, _) = register(&server, &mut bob, "bob", "b1", false).await;
    let (_carol_id, _) = register(&server, &mut carol, "carol", "c1", false).await;
    let (_alice_conn, mut alice_rx) = server.connections().register(&alice_id, "a1", 16);
    let (bob_conn_offline, _bob_rx_offline) = server.connections().register(&bob_id, "b1", 16);
    let chat = create_direct_chat(&server, &mut alice, "bob", proto::E2eMode::NoneDev).await;

    // Собеседник офлайн: приглашение отклоняется без создания звонка.
    server.connections().unregister(bob_conn_offline);
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::CallInvite(proto::CallInviteRequest {
                chat_id: chat.chat_id.clone(),
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::Conflict as i32);
    assert!(error.message.contains("offline"), "message: {error:?}");

    // Возвращаем Bob в сеть; отклонение доступно только callee.
    let (_bob_conn, mut bob_rx) = server.connections().register(&bob_id, "b1", 16);
    let ringing = invite(&server, &mut alice, &chat.chat_id).await;
    let event = next_call_event(&mut bob_rx).await;
    assert_eq!(call_event_kind(&event), proto::CallEventKind::Invited);

    // Второй звонок при занятых участниках запрещён.
    let error = expect_error(
        call(
            &server,
            &mut bob,
            Method::CallInvite(proto::CallInviteRequest {
                chat_id: chat.chat_id.clone(),
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::Conflict as i32);

    // Посторонний не может завершить чужой звонок.
    let error = expect_error(
        call(
            &server,
            &mut carol,
            Method::CallEnd(proto::CallEndRequest {
                call_id: ringing.call_id.clone(),
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::Forbidden as i32);

    let declined = expect_call(
        call(
            &server,
            &mut bob,
            Method::CallDecline(proto::CallDeclineRequest {
                call_id: ringing.call_id,
            }),
        )
        .await,
    );
    assert_eq!(declined.state, proto::CallState::Ringing as i32);
    let event = next_call_event(&mut alice_rx).await;
    assert_eq!(call_event_kind(&event), proto::CallEventKind::Declined);
}

#[tokio::test]
async fn calls_are_rejected_for_groups_and_unknown_ids() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let (alice_id, _) = register(&server, &mut alice, "alice", "a1", false).await;
    let _ = register(&server, &mut ConnState::new(peer()), "bob", "b1", false).await;
    let (_conn, _rx) = server.connections().register(&alice_id, "a1", 16);

    // Групповой чат: звонки запрещены.
    let result = call(
        &server,
        &mut alice,
        Method::CreateGroupChat(proto::CreateGroupChatRequest {
            name: "Группа".to_string(),
            member_usernames: vec!["bob".to_string()],
            e2e_mode: proto::E2eMode::NoneDev as i32,
        }),
    )
    .await;
    let group = match result {
        R::CreateGroupChat(response) => response.chat.expect("group chat"),
        other => panic!("create group failed: {other:?}"),
    };
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::CallInvite(proto::CallInviteRequest {
                chat_id: group.chat_id,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);

    // Неизвестный call_id.
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::CallAccept(proto::CallAcceptRequest {
                call_id: "does-not-exist".to_string(),
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::NotFound as i32);
}

#[tokio::test]
async fn invite_is_rate_limited() {
    let server = test_server_with(|config| {
        config.rate_limits.call_invite_per_minute = 1;
    })
    .await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    let (alice_id, _) = register(&server, &mut alice, "alice", "a1", false).await;
    let (bob_id, _) = register(&server, &mut bob, "bob", "b1", false).await;
    let chat = create_direct_chat(&server, &mut alice, "bob", proto::E2eMode::NoneDev).await;
    // Каналы нужно удерживать: закрытый push-канал считается офлайном.
    let (_alice_conn, _alice_rx) = server.connections().register(&alice_id, "a1", 16);
    let (_bob_conn, _bob_rx) = server.connections().register(&bob_id, "b1", 16);

    let ringing = invite(&server, &mut alice, &chat.chat_id).await;
    expect_call(
        call(
            &server,
            &mut alice,
            Method::CallEnd(proto::CallEndRequest {
                call_id: ringing.call_id,
            }),
        )
        .await,
    );

    // Первое приглашение израсходовало окно — даже освободившись, звонить
    // повторно сразу нельзя.
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::CallInvite(proto::CallInviteRequest {
                chat_id: chat.chat_id,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::RateLimited as i32);
}

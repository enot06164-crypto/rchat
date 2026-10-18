//! Интеграционные тесты сервисного слоя поверх SQLite: регистрация, вход,
//! чаты, сообщения, синхронизация, receipts, snapshot и коды ошибок.
//!
//! Тесты вызывают [`Server::handle_request`] напрямую, без сети: gateway,
//! TLS и WebSocket проверяются в e2e-тестах.

mod common;

use common::*;
use rchat_proto::proto;
use rchat_proto::proto::request::Method;
use rchat_proto::proto::response::Result as R;
use rchat_proto::proto::sync_event::Payload;
use rchat_server::service::ConnState;

#[tokio::test]
async fn full_plaintext_flow_registration_to_receipt() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());

    let (alice_id, _) = register(&server, &mut alice, "alice", "a1", false).await;
    let (bob_id, _) = register(&server, &mut bob, "bob", "b1", false).await;

    // Чат для пары создаётся один раз: повтор возвращает тот же chat_id.
    let chat = create_direct_chat(&server, &mut alice, "bob", proto::E2eMode::NoneDev).await;
    assert_eq!(chat.members.len(), 2);
    let same = create_direct_chat(&server, &mut bob, "alice", proto::E2eMode::NoneDev).await;
    assert_eq!(chat.chat_id, same.chat_id);

    // Сообщение.
    let sent = expect_sent(
        send_message(
            &server,
            &mut alice,
            proto::SendMessageRequest {
                chat_id: chat.chat_id.clone(),
                client_message_id: "m1".to_string(),
                content_kind: "text".to_string(),
                reply_to_server_id: 0,
                recipients: Vec::new(),
                plaintext_dev: "привет".to_string(),
                ..Default::default()
            },
        )
        .await,
    );
    assert_eq!(sent.server_id, 1);
    assert_eq!(sent.plaintext_dev, "привет");
    assert_eq!(sent.sender_device_id, "a1");
    assert_eq!(sent.sender_user_id, alice_id);

    // Повтор с тем же client_message_id — та же запись, без дублей.
    let again = expect_sent(
        send_message(
            &server,
            &mut alice,
            proto::SendMessageRequest {
                chat_id: chat.chat_id.clone(),
                client_message_id: "m1".to_string(),
                content_kind: "text".to_string(),
                reply_to_server_id: 0,
                recipients: Vec::new(),
                plaintext_dev: "другое содержимое".to_string(),
                ..Default::default()
            },
        )
        .await,
    );
    assert_eq!(again.server_id, sent.server_id);
    assert_eq!(again.plaintext_dev, "привет");

    // Журнал bob: chat_created, затем message.
    let page = sync(&server, &mut bob, 0).await;
    assert_eq!(page.from_seq, 1);
    assert_eq!(page.up_to_seq, 2);
    assert!(!page.resync_required);
    assert_eq!(page.events.len(), 2);
    assert_eq!(page.events[0].seq, 1);
    assert_eq!(page.events[0].kind_hint, 10);
    assert!(matches!(
        page.events[0].payload.as_ref(),
        Some(Payload::ChatCreated(_))
    ));
    assert_eq!(page.events[1].seq, 2);
    match page.events[1].payload.as_ref() {
        Some(Payload::Message(event)) => {
            let message = event.message.as_ref().expect("message payload");
            assert_eq!(message.server_id, 1);
            assert_eq!(message.plaintext_dev, "привет");
            assert_eq!(message.sender_user_id, alice_id);
        }
        other => panic!("expected message event, got {other:?}"),
    }

    // Хвост журнала пуст, курсор на верхней границе.
    let tail = sync(&server, &mut bob, 2).await;
    assert!(tail.events.is_empty());
    assert_eq!(tail.up_to_seq, 2);
    assert!(!tail.resync_required);

    // История видна bob'у.
    let history = match call(
        &server,
        &mut bob,
        Method::GetHistory(proto::GetHistoryRequest {
            chat_id: chat.chat_id.clone(),
            before_server_id: 0,
            limit: 50,
        }),
    )
    .await
    {
        R::GetHistory(response) => response,
        other => panic!("history failed: {other:?}"),
    };
    assert_eq!(history.messages.len(), 1);
    assert_eq!(history.messages[0].plaintext_dev, "привет");
    assert!(!history.has_more);

    // Подтверждение прочтения от bob → событие receipt у alice.
    let receipt = call(
        &server,
        &mut bob,
        Method::MarkReceipt(proto::MarkReceiptRequest {
            chat_id: chat.chat_id.clone(),
            up_to_server_id: 1,
            read: true,
        }),
    )
    .await;
    assert!(matches!(receipt, R::MarkReceipt(_)));

    // У alice: seq1 chat_created, seq2 её собственное сообщение,
    // seq3 receipt от bob. Читаем после первого события.
    let alice_page = sync(&server, &mut alice, 1).await;
    assert_eq!(alice_page.events.len(), 2);
    assert_eq!(alice_page.events[0].kind_hint, 11);
    assert_eq!(alice_page.events[1].kind_hint, 14);
    match alice_page.events[1].payload.as_ref() {
        Some(Payload::Receipt(event)) => {
            assert_eq!(event.user_id, bob_id);
            assert_eq!(event.chat_id, chat.chat_id);
            assert_eq!(event.up_to_server_id, 1);
            assert!(event.read);
        }
        other => panic!("expected receipt event, got {other:?}"),
    }

    // Повторный receipt с меньшим значением не откатывает прогресс
    // и не плодит события.
    let repeated = call(
        &server,
        &mut bob,
        Method::MarkReceipt(proto::MarkReceiptRequest {
            chat_id: chat.chat_id.clone(),
            up_to_server_id: 0,
            read: true,
        }),
    )
    .await;
    assert!(matches!(repeated, R::MarkReceipt(_)));
    let alice_tail = sync(&server, &mut alice, 3).await;
    assert!(alice_tail.events.is_empty());

    // GetMe возвращает владельца сессии.
    match call(&server, &mut bob, Method::GetMe(proto::GetMeRequest {})).await {
        R::GetMe(response) => {
            assert_eq!(response.user.expect("user").username, "bob");
            assert_eq!(response.device.expect("device").device_id, "b1");
        }
        other => panic!("get_me failed: {other:?}"),
    }

    // Сообщение в несуществующий чат — not found.
    let error = expect_error(
        send_message(
            &server,
            &mut alice,
            proto::SendMessageRequest {
                chat_id: "no-such-chat".to_string(),
                client_message_id: "m2".to_string(),
                content_kind: "text".to_string(),
                reply_to_server_id: 0,
                recipients: Vec::new(),
                plaintext_dev: "привет".to_string(),
                ..Default::default()
            },
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::NotFound as i32);

    // Невалидный chat_id отвергается до обращения к БД.
    let error = expect_error(
        send_message(
            &server,
            &mut alice,
            proto::SendMessageRequest {
                chat_id: String::new(),
                client_message_id: "m2".to_string(),
                content_kind: "text".to_string(),
                reply_to_server_id: 0,
                recipients: Vec::new(),
                plaintext_dev: "привет".to_string(),
                ..Default::default()
            },
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);
}

#[tokio::test]
async fn plaintext_chats_can_be_disabled() {
    let server = test_server_with(|config| config.allow_dev_plaintext_chats = false).await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    register(&server, &mut alice, "alice", "a1", false).await;
    register(&server, &mut bob, "bob", "b1", false).await;

    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::CreateDirectChat(proto::CreateDirectChatRequest {
                peer_username: "bob".to_string(),
                e2e_mode: proto::E2eMode::NoneDev as i32,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::Forbidden as i32);

    // Режим обязан указываться явно.
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::CreateDirectChat(proto::CreateDirectChatRequest {
                peer_username: "bob".to_string(),
                e2e_mode: proto::E2eMode::Unspecified as i32,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);
}

#[tokio::test]
async fn e2e_chat_requires_keys_and_full_coverage() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    register(&server, &mut alice, "alice", "a1", false).await;
    register(&server, &mut bob, "bob", "b1", false).await;

    // Оба без ключей: E2E-чат невозможен.
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::CreateDirectChat(proto::CreateDirectChatRequest {
                peer_username: "bob".to_string(),
                e2e_mode: proto::E2eMode::OlmV1 as i32,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::E2eKeysMissing as i32);

    // Устройства с ключами: чат создаётся.
    let mut alice_e2e = ConnState::new(peer());
    let mut bob_e2e = ConnState::new(peer());
    register(&server, &mut alice_e2e, "carol", "c1", true).await;
    register(&server, &mut bob_e2e, "dave", "d1", true).await;
    let chat = create_direct_chat(&server, &mut alice_e2e, "dave", proto::E2eMode::OlmV1).await;

    // Плейнтекст в E2E-чате запрещён.
    let error = expect_error(
        send_message(
            &server,
            &mut alice_e2e,
            proto::SendMessageRequest {
                chat_id: chat.chat_id.clone(),
                client_message_id: "e1".to_string(),
                content_kind: "text".to_string(),
                reply_to_server_id: 0,
                recipients: Vec::new(),
                plaintext_dev: "нельзя".to_string(),
                ..Default::default()
            },
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::E2eRequired as i32);

    // Шифртекст обязателен для всех E2E-устройств получателей.
    let error = expect_error(
        send_message(
            &server,
            &mut alice_e2e,
            proto::SendMessageRequest {
                chat_id: chat.chat_id.clone(),
                client_message_id: "e1".to_string(),
                content_kind: "text".to_string(),
                reply_to_server_id: 0,
                recipients: Vec::new(),
                plaintext_dev: String::new(),
                ..Default::default()
            },
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::E2eKeysMissing as i32);

    // Полное покрытие: копия для d1.
    let sent = expect_sent(
        send_message(
            &server,
            &mut alice_e2e,
            proto::SendMessageRequest {
                chat_id: chat.chat_id.clone(),
                client_message_id: "e1".to_string(),
                content_kind: "text".to_string(),
                reply_to_server_id: 0,
                recipients: vec![proto::RecipientPayload {
                    device_id: "d1".to_string(),
                    ciphertext: vec![0xAB; 64],
                    e2e_version: 1,
                    olm_type: proto::OlmMessageType::Prekey as i32,
                    sender_curve25519: vec![0x22; 32],
                    ..Default::default()
                }],
                plaintext_dev: String::new(),
                ..Default::default()
            },
        )
        .await,
    );
    // server_id монотонен; отклонённые попытки могли занять номера до него.
    assert!(sent.server_id > 0);
    assert!(sent.plaintext_dev.is_empty());
    // В ответе отправителю его собственной копии нет: ciphertext приходит
    // только получателю (проверяется в тесте fan-out ниже).
    assert!(sent.ciphertext.is_empty());

    // Копия для отправителя в recipients запрещена.
    let error = expect_error(
        send_message(
            &server,
            &mut alice_e2e,
            proto::SendMessageRequest {
                chat_id: chat.chat_id.clone(),
                client_message_id: "e2".to_string(),
                content_kind: "text".to_string(),
                reply_to_server_id: 0,
                recipients: vec![
                    proto::RecipientPayload {
                        device_id: "d1".to_string(),
                        ciphertext: vec![0xAB; 64],
                        e2e_version: 1,
                        olm_type: proto::OlmMessageType::Normal as i32,
                        sender_curve25519: vec![0x22; 32],
                        ..Default::default()
                    },
                    proto::RecipientPayload {
                        device_id: "c1".to_string(),
                        ciphertext: vec![0xCD; 64],
                        e2e_version: 1,
                        olm_type: proto::OlmMessageType::Normal as i32,
                        sender_curve25519: vec![0x22; 32],
                        ..Default::default()
                    },
                ],
                plaintext_dev: String::new(),
                ..Default::default()
            },
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);
}

#[tokio::test]
async fn multi_device_login_and_fanout() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob1 = ConnState::new(peer());
    let mut bob2 = ConnState::new(peer());
    register(&server, &mut alice, "alice", "a1", true).await;
    register(&server, &mut bob1, "bob", "b1", true).await;

    // Второе устройство входит под тем же пользователем.
    let login = call(
        &server,
        &mut bob2,
        Method::Login(proto::LoginRequest {
            username: "bob".to_string(),
            password: "correct horse battery staple".to_string(),
            device: Some(device("b2", true)),
            one_time_keys: Vec::new(),
            fallback_key: None,
        }),
    )
    .await;
    match login {
        R::Login(response) => {
            assert_eq!(response.devices.len(), 2);
        }
        other => panic!("login failed: {other:?}"),
    }

    let chat = create_direct_chat(&server, &mut alice, "bob", proto::E2eMode::OlmV1).await;

    // Копии нужны для обоих устройств bob.
    let error = expect_error(
        send_message(
            &server,
            &mut alice,
            proto::SendMessageRequest {
                chat_id: chat.chat_id.clone(),
                client_message_id: "e1".to_string(),
                content_kind: "text".to_string(),
                reply_to_server_id: 0,
                recipients: vec![proto::RecipientPayload {
                    device_id: "b1".to_string(),
                    ciphertext: vec![0xAB; 64],
                    e2e_version: 1,
                    olm_type: proto::OlmMessageType::Prekey as i32,
                    sender_curve25519: vec![0x22; 32],
                    ..Default::default()
                }],
                plaintext_dev: String::new(),
                ..Default::default()
            },
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::E2eKeysMissing as i32);

    let sent = expect_sent(
        send_message(
            &server,
            &mut alice,
            proto::SendMessageRequest {
                chat_id: chat.chat_id.clone(),
                client_message_id: "e1".to_string(),
                content_kind: "text".to_string(),
                reply_to_server_id: 0,
                recipients: vec![
                    proto::RecipientPayload {
                        device_id: "b1".to_string(),
                        ciphertext: vec![0xAB; 64],
                        e2e_version: 1,
                        olm_type: proto::OlmMessageType::Prekey as i32,
                        sender_curve25519: vec![0x22; 32],
                        ..Default::default()
                    },
                    proto::RecipientPayload {
                        device_id: "b2".to_string(),
                        ciphertext: vec![0xCD; 64],
                        e2e_version: 1,
                        olm_type: proto::OlmMessageType::Prekey as i32,
                        sender_curve25519: vec![0x22; 32],
                        ..Default::default()
                    },
                ],
                plaintext_dev: String::new(),
                ..Default::default()
            },
        )
        .await,
    );
    assert!(sent.server_id > 0);

    // Оба устройства получают событие с собственной копией шифртекста.
    // В журнале каждого есть и свои события (device_changed при входе b2),
    // поэтому ищем именно событие сообщения.
    let message_event = |page: &proto::SyncEventsResponse| -> proto::Message {
        page.events
            .iter()
            .rev()
            .find_map(|event| match event.payload.as_ref() {
                Some(Payload::Message(message)) => message.message.clone(),
                _ => None,
            })
            .expect("message event in journal")
    };
    let page1 = sync(&server, &mut bob1, 0).await;
    let view1 = message_event(&page1);
    assert_eq!(view1.ciphertext, vec![0xAB; 64]);
    assert_eq!(view1.olm_type, proto::OlmMessageType::Prekey as i32);
    assert!(view1.plaintext_dev.is_empty());
    let page2 = sync(&server, &mut bob2, 0).await;
    let view2 = message_event(&page2);
    assert_eq!(view2.ciphertext, vec![0xCD; 64]);
}

#[tokio::test]
async fn snapshot_returns_state_with_upper_bound() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    register(&server, &mut alice, "alice", "a1", false).await;
    register(&server, &mut bob, "bob", "b1", false).await;
    let chat = create_direct_chat(&server, &mut alice, "bob", proto::E2eMode::NoneDev).await;
    expect_sent(
        send_message(
            &server,
            &mut alice,
            proto::SendMessageRequest {
                chat_id: chat.chat_id.clone(),
                client_message_id: "m1".to_string(),
                content_kind: "text".to_string(),
                reply_to_server_id: 0,
                recipients: Vec::new(),
                plaintext_dev: "привет".to_string(),
                ..Default::default()
            },
        )
        .await,
    );

    let snapshot = match call(
        &server,
        &mut bob,
        Method::GetSnapshot(proto::SnapshotRequest {}),
    )
    .await
    {
        R::GetSnapshot(response) => response,
        other => panic!("snapshot failed: {other:?}"),
    };
    assert_eq!(snapshot.up_to_seq, 2);
    assert_eq!(snapshot.chats.len(), 1);
    assert_eq!(snapshot.chats[0].chat_id, chat.chat_id);
    assert_eq!(snapshot.devices.len(), 1);
    assert_eq!(snapshot.devices[0].device_id, "b1");
    assert_eq!(snapshot.users.len(), 2);

    // После snapshot клиент продолжает строго с up_to_seq.
    let tail = sync(&server, &mut bob, snapshot.up_to_seq).await;
    assert!(tail.events.is_empty());
}

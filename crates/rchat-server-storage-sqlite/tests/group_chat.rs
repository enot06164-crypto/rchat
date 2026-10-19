//! Интеграционные тесты групповых чатов: создание, приглашения, выход,
//! исключение, переименование, роли, лимиты и групповой E2E-конверт
//! (megolm-поля сервер валидирует, не расшифровывая).

mod common;

use common::*;
use rchat_proto::proto;
use rchat_proto::proto::request::Method;
use rchat_proto::proto::response::Result as R;
use rchat_proto::proto::sync_event::Payload;
use rchat_server::service::{ConnState, Server};

/// Создаёт групповой чат, паникует при ошибке.
async fn create_group(
    server: &Server,
    state: &mut ConnState,
    name: &str,
    members: &[&str],
    mode: proto::E2eMode,
) -> proto::Chat {
    let result = call(
        server,
        state,
        Method::CreateGroupChat(proto::CreateGroupChatRequest {
            name: name.to_string(),
            member_usernames: members.iter().map(|m| m.to_string()).collect(),
            e2e_mode: mode as i32,
        }),
    )
    .await;
    match result {
        R::CreateGroupChat(response) => response.chat.expect("chat in response"),
        other => panic!("create group failed: {other:?}"),
    }
}

async fn invite(server: &Server, state: &mut ConnState, chat_id: &str, usernames: &[&str]) -> R {
    call(
        server,
        state,
        Method::InviteToChat(proto::InviteToChatRequest {
            chat_id: chat_id.to_string(),
            usernames: usernames.iter().map(|u| u.to_string()).collect(),
        }),
    )
    .await
}

async fn list_chats(server: &Server, state: &mut ConnState) -> Vec<proto::Chat> {
    match call(server, state, Method::ListChats(proto::ListChatsRequest {})).await {
        R::ListChats(response) => response.chats,
        other => panic!("list chats failed: {other:?}"),
    }
}

async fn get_devices(
    server: &Server,
    state: &mut ConnState,
    user_ids: &[String],
) -> Vec<proto::DeviceInfo> {
    match call(
        server,
        state,
        Method::GetDevices(proto::GetDevicesRequest {
            user_ids: user_ids.to_vec(),
            include_own: false,
        }),
    )
    .await
    {
        R::GetDevices(response) => response.devices,
        other => panic!("get devices failed: {other:?}"),
    }
}

/// Данные для группового E2E-сообщения: общий ciphertext + доставка ключа.
fn group_send_request(
    chat_id: &str,
    client_id: &str,
    group_ciphertext: Vec<u8>,
    session_id: &str,
    key_recipients: &[(&str, Vec<u8>)],
) -> proto::SendMessageRequest {
    proto::SendMessageRequest {
        chat_id: chat_id.to_string(),
        client_message_id: client_id.to_string(),
        content_kind: "text".to_string(),
        grouped_id: String::new(),
        reply_to_server_id: 0,
        recipients: key_recipients
            .iter()
            .map(|(device_id, ciphertext)| proto::RecipientPayload {
                device_id: device_id.to_string(),
                ciphertext: ciphertext.clone(),
                e2e_version: rchat_proto::E2E_VERSION_OLM_V1,
                olm_type: proto::OlmMessageType::Prekey as i32,
                sender_curve25519: vec![0x22; 32],
                payload_kind: proto::RecipientPayloadKind::GroupKey as i32,
            })
            .collect(),
        plaintext_dev: String::new(),
        group_ciphertext,
        group_session_id: session_id.to_string(),
    }
}

/// Все сообщения из страницы журнала (kind_hint = 11).
fn message_views(page: &proto::SyncEventsResponse) -> Vec<proto::Message> {
    page.events
        .iter()
        .filter(|event| event.kind_hint == 11)
        .map(|event| match event.payload.as_ref() {
            Some(Payload::Message(inner)) => inner.message.clone().expect("message"),
            other => panic!("expected message event, got {other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn create_group_chat_roles_and_snapshot() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    let (alice_id, _) = register(&server, &mut alice, "alice", "a1", false).await;
    let (bob_id, _) = register(&server, &mut bob, "bob", "b1", false).await;

    // Имя нормализуется (trim), роли расставляются при создании.
    let chat = create_group(
        &server,
        &mut alice,
        "  Команда  ",
        &["bob"],
        proto::E2eMode::NoneDev,
    )
    .await;
    assert_eq!(chat.kind, proto::ChatKind::Group as i32);
    assert_eq!(chat.name, "Команда");
    assert_eq!(chat.members.len(), 2);
    let owner = chat
        .members
        .iter()
        .find(|m| m.role == proto::ChatMemberRole::Owner as i32)
        .expect("owner");
    assert_eq!(owner.user_id, alice_id);
    assert_eq!(owner.username, "alice");
    let member = chat
        .members
        .iter()
        .find(|m| m.role == proto::ChatMemberRole::Member as i32)
        .expect("member");
    assert_eq!(member.user_id, bob_id);

    // Второй участник видит чат в списке и получает событие chat_created.
    let bob_chats = list_chats(&server, &mut bob).await;
    assert_eq!(bob_chats.len(), 1);
    assert_eq!(bob_chats[0].name, "Команда");
    assert_eq!(bob_chats[0].members.len(), 2);

    let page = sync(&server, &mut bob, 0).await;
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].kind_hint, 10);
    match page.events[0].payload.as_ref() {
        Some(Payload::ChatCreated(event)) => {
            let snapshot = event.chat.as_ref().expect("chat");
            assert_eq!(snapshot.chat_id, chat.chat_id);
            assert_eq!(snapshot.members.len(), 2);
        }
        other => panic!("expected chat_created, got {other:?}"),
    }

    // Snapshot несёт имя и роли.
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
    assert_eq!(snapshot.chats.len(), 1);
    assert_eq!(snapshot.chats[0].name, "Команда");
    assert!(snapshot.chats[0]
        .members
        .iter()
        .any(|m| m.role == proto::ChatMemberRole::Owner as i32 && m.user_id == alice_id));
}

#[tokio::test]
async fn create_group_validations() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut dave = ConnState::new(peer());
    register(&server, &mut alice, "alice", "a1", true).await;
    register(&server, &mut dave, "dave", "d1", false).await;

    // Пустое (после trim) имя.
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::CreateGroupChat(proto::CreateGroupChatRequest {
                name: "   ".to_string(),
                member_usernames: Vec::new(),
                e2e_mode: proto::E2eMode::NoneDev as i32,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);

    // Неизвестный пользователь.
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::CreateGroupChat(proto::CreateGroupChatRequest {
                name: "G".to_string(),
                member_usernames: vec!["ghost".to_string()],
                e2e_mode: proto::E2eMode::NoneDev as i32,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::NotFound as i32);

    // Режим обязан быть указан явно.
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::CreateGroupChat(proto::CreateGroupChatRequest {
                name: "G".to_string(),
                member_usernames: Vec::new(),
                e2e_mode: proto::E2eMode::Unspecified as i32,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);

    // E2E-группа требует устройств с ключами у всех участников.
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::CreateGroupChat(proto::CreateGroupChatRequest {
                name: "G".to_string(),
                member_usernames: vec!["dave".to_string()],
                e2e_mode: proto::E2eMode::OlmV1 as i32,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::E2eKeysMissing as i32);

    // В dev-группу участник без ключей допустим; себя можно перечислить —
    // дубликат молча пропускается.
    let chat = create_group(
        &server,
        &mut alice,
        "Dev",
        &["dave", "alice", "dave"],
        proto::E2eMode::NoneDev,
    )
    .await;
    assert_eq!(chat.members.len(), 2);
}

#[tokio::test]
async fn invite_member_and_chat_updated() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    let mut carol = ConnState::new(peer());
    let mut dave = ConnState::new(peer());
    register(&server, &mut alice, "alice", "a1", false).await;
    register(&server, &mut bob, "bob", "b1", false).await;
    register(&server, &mut carol, "carol", "c1", false).await;
    register(&server, &mut dave, "dave", "d1", false).await;
    let chat = create_group(&server, &mut alice, "G", &["bob"], proto::E2eMode::NoneDev).await;

    // Приглашать может любой участник, не только владелец.
    match invite(&server, &mut bob, &chat.chat_id, &["carol"]).await {
        R::InviteToChat(response) => {
            let updated = response.chat.expect("chat");
            assert_eq!(updated.members.len(), 3);
        }
        other => panic!("invite failed: {other:?}"),
    }
    assert_eq!(list_chats(&server, &mut carol).await.len(), 1);

    // alice получает chat_updated (её журнал: seq1 chat_created, seq2 update).
    let alice_page = sync(&server, &mut alice, 1).await;
    assert_eq!(alice_page.events.len(), 1);
    assert_eq!(alice_page.events[0].kind_hint, 16);
    match alice_page.events[0].payload.as_ref() {
        Some(Payload::ChatUpdated(event)) => {
            assert_eq!(event.chat.as_ref().expect("chat").members.len(), 3);
        }
        other => panic!("expected chat_updated, got {other:?}"),
    }

    // У carol журнал начинается прямо с chat_updated.
    let carol_page = sync(&server, &mut carol, 0).await;
    assert_eq!(carol_page.events.len(), 1);
    assert_eq!(carol_page.events[0].kind_hint, 16);

    // Повторное приглашение уже участника идемпотентно: чат тот же,
    // новых событий нет.
    match invite(&server, &mut bob, &chat.chat_id, &["carol", "bob"]).await {
        R::InviteToChat(response) => {
            assert_eq!(response.chat.expect("chat").members.len(), 3);
        }
        other => panic!("invite failed: {other:?}"),
    }
    let carol_tail = sync(&server, &mut carol, 1).await;
    assert!(carol_tail.events.is_empty());

    // Посторонний приглашать не может.
    let error = expect_error(invite(&server, &mut dave, &chat.chat_id, &["carol"]).await);
    assert_eq!(error.code, proto::ErrorCode::Forbidden as i32);

    // Приглашение в личный чат запрещено.
    let direct = create_direct_chat(&server, &mut alice, "dave", proto::E2eMode::NoneDev).await;
    let error = expect_error(invite(&server, &mut alice, &direct.chat_id, &["carol"]).await);
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);
}

#[tokio::test]
async fn remove_member_owner_only() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    let mut carol = ConnState::new(peer());
    let (alice_id, _) = register(&server, &mut alice, "alice", "a1", false).await;
    let (_bob_id, _) = register(&server, &mut bob, "bob", "b1", false).await;
    let (carol_id, _) = register(&server, &mut carol, "carol", "c1", false).await;
    let chat = create_group(
        &server,
        &mut alice,
        "G",
        &["bob", "carol"],
        proto::E2eMode::NoneDev,
    )
    .await;

    // До группы bob видел устройства carol; после исключения — нет.
    assert_eq!(
        get_devices(&server, &mut bob, std::slice::from_ref(&carol_id))
            .await
            .len(),
        1
    );

    // Участник (не владелец) исключать не может.
    let error = expect_error(
        call(
            &server,
            &mut bob,
            Method::RemoveChatMember(proto::RemoveChatMemberRequest {
                chat_id: chat.chat_id.clone(),
                user_id: carol_id.clone(),
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::Forbidden as i32);

    // Себя владелец через remove не исключает — для этого leave.
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::RemoveChatMember(proto::RemoveChatMemberRequest {
                chat_id: chat.chat_id.clone(),
                user_id: alice_id,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);

    // Неизвестный участник.
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::RemoveChatMember(proto::RemoveChatMemberRequest {
                chat_id: chat.chat_id.clone(),
                user_id: "no-such-user".to_string(),
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::NotFound as i32);

    // Владелец исключает carol.
    match call(
        &server,
        &mut alice,
        Method::RemoveChatMember(proto::RemoveChatMemberRequest {
            chat_id: chat.chat_id.clone(),
            user_id: carol_id.clone(),
        }),
    )
    .await
    {
        R::RemoveChatMember(response) => {
            let updated = response.chat.expect("chat");
            assert_eq!(updated.members.len(), 2);
            assert!(updated.members.iter().all(|m| m.user_id != carol_id));
        }
        other => panic!("remove failed: {other:?}"),
    }

    // Исключённая узнаёт об этом из chat_updated (её журнал: seq1 create).
    let carol_page = sync(&server, &mut carol, 1).await;
    assert_eq!(carol_page.events.len(), 1);
    assert_eq!(carol_page.events[0].kind_hint, 16);
    match carol_page.events[0].payload.as_ref() {
        Some(Payload::ChatUpdated(event)) => {
            let snapshot = event.chat.as_ref().expect("chat");
            assert!(snapshot.members.iter().all(|m| m.user_id != carol_id));
        }
        other => panic!("expected chat_updated, got {other:?}"),
    }

    // Доступ отозван: чат не виден, отправка и история — forbidden,
    // устройства carol больше не видны участникам.
    assert!(list_chats(&server, &mut carol).await.is_empty());
    let error = expect_error(
        send_message(
            &server,
            &mut carol,
            proto::SendMessageRequest {
                chat_id: chat.chat_id.clone(),
                client_message_id: "m1".to_string(),
                content_kind: "text".to_string(),
                grouped_id: String::new(),
                reply_to_server_id: 0,
                plaintext_dev: "привет".to_string(),
                ..Default::default()
            },
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::Forbidden as i32);
    let error = expect_error(
        call(
            &server,
            &mut carol,
            Method::GetHistory(proto::GetHistoryRequest {
                chat_id: chat.chat_id.clone(),
                before_server_id: 0,
                limit: 0,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::Forbidden as i32);
    assert!(get_devices(&server, &mut bob, &[carol_id]).await.is_empty());
}

#[tokio::test]
async fn leave_chat_transfers_ownership() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    let mut carol = ConnState::new(peer());
    register(&server, &mut alice, "alice", "a1", false).await;
    register(&server, &mut bob, "bob", "b1", false).await;
    register(&server, &mut carol, "carol", "c1", false).await;
    let chat = create_group(&server, &mut alice, "G", &["bob"], proto::E2eMode::NoneDev).await;
    assert!(matches!(
        invite(&server, &mut bob, &chat.chat_id, &["carol"]).await,
        R::InviteToChat(_)
    ));

    // Обычный участник выходит: состав обновляется, доступ снимается.
    assert!(matches!(
        call(
            &server,
            &mut carol,
            Method::LeaveChat(proto::LeaveChatRequest {
                chat_id: chat.chat_id.clone(),
            }),
        )
        .await,
        R::LeaveChat(_)
    ));
    assert!(list_chats(&server, &mut carol).await.is_empty());

    // Владелец выходит: владение переходит старейшему участнику (bob).
    assert!(matches!(
        call(
            &server,
            &mut alice,
            Method::LeaveChat(proto::LeaveChatRequest {
                chat_id: chat.chat_id.clone(),
            }),
        )
        .await,
        R::LeaveChat(_)
    ));
    assert!(list_chats(&server, &mut alice).await.is_empty());
    let bob_chats = list_chats(&server, &mut bob).await;
    assert_eq!(bob_chats.len(), 1);
    assert_eq!(bob_chats[0].members.len(), 1);
    let bob_member = &bob_chats[0].members[0];
    assert_eq!(bob_member.role, proto::ChatMemberRole::Owner as i32);

    // В личном чате выхода нет.
    let mut dave = ConnState::new(peer());
    register(&server, &mut dave, "dave", "d1", false).await;
    let direct = create_direct_chat(&server, &mut bob, "dave", proto::E2eMode::NoneDev).await;
    let error = expect_error(
        call(
            &server,
            &mut bob,
            Method::LeaveChat(proto::LeaveChatRequest {
                chat_id: direct.chat_id,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);

    // Выход из несуществующего чата.
    let error = expect_error(
        call(
            &server,
            &mut bob,
            Method::LeaveChat(proto::LeaveChatRequest {
                chat_id: "no-such-chat".to_string(),
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::NotFound as i32);
}

#[tokio::test]
async fn rename_group_owner_only() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    register(&server, &mut alice, "alice", "a1", false).await;
    register(&server, &mut bob, "bob", "b1", false).await;
    let chat = create_group(&server, &mut alice, "G", &["bob"], proto::E2eMode::NoneDev).await;

    // Участник переименовать не может.
    let error = expect_error(
        call(
            &server,
            &mut bob,
            Method::RenameChat(proto::RenameChatRequest {
                chat_id: chat.chat_id.clone(),
                name: "H".to_string(),
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::Forbidden as i32);

    // Невалидное имя отвергается.
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::RenameChat(proto::RenameChatRequest {
                chat_id: chat.chat_id.clone(),
                name: "  ".to_string(),
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);

    // Владелец переименовывает; bob получает chat_updated с новым именем.
    match call(
        &server,
        &mut alice,
        Method::RenameChat(proto::RenameChatRequest {
            chat_id: chat.chat_id.clone(),
            name: "Новое имя".to_string(),
        }),
    )
    .await
    {
        R::RenameChat(response) => {
            assert_eq!(response.chat.expect("chat").name, "Новое имя");
        }
        other => panic!("rename failed: {other:?}"),
    }
    let bob_page = sync(&server, &mut bob, 1).await;
    assert_eq!(bob_page.events.len(), 1);
    assert_eq!(bob_page.events[0].kind_hint, 16);
    match bob_page.events[0].payload.as_ref() {
        Some(Payload::ChatUpdated(event)) => {
            assert_eq!(event.chat.as_ref().expect("chat").name, "Новое имя");
        }
        other => panic!("expected chat_updated, got {other:?}"),
    }

    // Повторное имя — идемпотентно, без новых событий.
    assert!(matches!(
        call(
            &server,
            &mut alice,
            Method::RenameChat(proto::RenameChatRequest {
                chat_id: chat.chat_id.clone(),
                name: "Новое имя".to_string(),
            }),
        )
        .await,
        R::RenameChat(_)
    ));
    let bob_tail = sync(&server, &mut bob, 2).await;
    assert!(bob_tail.events.is_empty());

    // Личный чат переименовать нельзя.
    let direct = create_direct_chat(&server, &mut alice, "bob", proto::E2eMode::NoneDev).await;
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::RenameChat(proto::RenameChatRequest {
                chat_id: direct.chat_id,
                name: "X".to_string(),
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);
}

#[tokio::test]
async fn group_member_limit_enforced() {
    let server = test_server_with(|config| config.max_group_members = 3).await;
    let mut alice = ConnState::new(peer());
    register(&server, &mut alice, "alice", "a1", false).await;
    for username in ["bob", "carol", "dave"] {
        let mut state = ConnState::new(peer());
        register(
            &server,
            &mut state,
            username,
            &format!("{username}-1"),
            false,
        )
        .await;
    }
    let chat = create_group(&server, &mut alice, "G", &["bob"], proto::E2eMode::NoneDev).await;

    // 2 → 3 участника: в пределах лимита.
    assert!(matches!(
        invite(&server, &mut alice, &chat.chat_id, &["carol"]).await,
        R::InviteToChat(_)
    ));
    // 3 → 4: превышение.
    let error = expect_error(invite(&server, &mut alice, &chat.chat_id, &["dave"]).await);
    assert_eq!(error.code, proto::ErrorCode::LimitExceeded as i32);

    // Создание сверх лимита отклоняется сразу.
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::CreateGroupChat(proto::CreateGroupChatRequest {
                name: "H".to_string(),
                member_usernames: vec!["bob".to_string(), "carol".to_string(), "dave".to_string()],
                e2e_mode: proto::E2eMode::NoneDev as i32,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::LimitExceeded as i32);
}

#[tokio::test]
async fn group_e2e_message_requires_megolm() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    register(&server, &mut alice, "alice", "a1", true).await;
    register(&server, &mut bob, "bob", "b1", true).await;
    let chat = create_group(
        &server,
        &mut alice,
        "Команда",
        &["bob"],
        proto::E2eMode::OlmV1,
    )
    .await;

    // Без group_ciphertext E2E-сообщение в группу не принимается.
    let error = expect_error(
        send_message(
            &server,
            &mut alice,
            group_send_request(&chat.chat_id, "m1", Vec::new(), "", &[]),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);

    // Ciphertext без идентификатора сессии.
    let error = expect_error(
        send_message(
            &server,
            &mut alice,
            group_send_request(&chat.chat_id, "m1", vec![1, 2, 3], "", &[]),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);

    // Получательская копия с видом MESSAGE в группе запрещена.
    let mut wrong_kind = group_send_request(&chat.chat_id, "m1", vec![7; 32], "s1", &[]);
    wrong_kind.recipients.push(proto::RecipientPayload {
        device_id: "b1".to_string(),
        ciphertext: vec![0xAA; 16],
        e2e_version: rchat_proto::E2E_VERSION_OLM_V1,
        olm_type: proto::OlmMessageType::Prekey as i32,
        sender_curve25519: vec![0x22; 32],
        payload_kind: proto::RecipientPayloadKind::Message as i32,
    });
    let error = expect_error(send_message(&server, &mut alice, wrong_kind).await);
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);

    // Полное покрытие не требуется: доставка ключа одному устройству.
    let sent = expect_sent(
        send_message(
            &server,
            &mut alice,
            group_send_request(
                &chat.chat_id,
                "m1",
                vec![7; 32],
                "session-1",
                &[("b1", vec![0xAA; 16])],
            ),
        )
        .await,
    );
    assert_eq!(sent.group_ciphertext, vec![7; 32]);
    assert_eq!(sent.group_session_id, "session-1");
    assert!(sent.ciphertext.is_empty(), "у отправителя копии нет");
    assert!(sent.plaintext_dev.is_empty());

    // Второе сообщение вовсе без копий: ключ у всех уже есть.
    expect_sent(
        send_message(
            &server,
            &mut alice,
            group_send_request(&chat.chat_id, "m2", vec![8; 32], "session-1", &[]),
        )
        .await,
    );

    // bob видит оба сообщения: общий megolm-ciphertext и свою копию ключа.
    let page = sync(&server, &mut bob, 0).await;
    let views = message_views(&page);
    assert_eq!(views.len(), 2);
    assert_eq!(views[0].group_ciphertext, vec![7; 32]);
    assert_eq!(views[0].group_session_id, "session-1");
    assert_eq!(views[0].ciphertext, vec![0xAA; 16]);
    assert_eq!(
        views[0].recipient_payload_kind,
        proto::RecipientPayloadKind::GroupKey as i32
    );
    assert_eq!(views[1].group_ciphertext, vec![8; 32]);
    assert!(
        views[1].ciphertext.is_empty(),
        "во втором ключ не рассылался"
    );

    // Устройство отправителя получает сообщение без копии, но с общим
    // ciphertext — расшифровка пойдёт через собственную групповую сессию.
    let alice_page = sync(&server, &mut alice, 1).await;
    let alice_views = message_views(&alice_page);
    assert_eq!(alice_views.len(), 2);
    assert_eq!(alice_views[0].group_ciphertext, vec![7; 32]);
    assert!(alice_views[0].ciphertext.is_empty());
}

#[tokio::test]
async fn group_e2e_rotation_on_member_removed() {
    // Серверный аспект ротации: после исключения участника его устройства
    // не видят ни ключей, ни сообщений нового состава.
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    let mut carol = ConnState::new(peer());
    register(&server, &mut alice, "alice", "a1", true).await;
    register(&server, &mut bob, "bob", "b1", true).await;
    let (carol_user, _) = register(&server, &mut carol, "carol", "c1", true).await;
    let chat = create_group(
        &server,
        &mut alice,
        "E2E",
        &["bob", "carol"],
        proto::E2eMode::OlmV1,
    )
    .await;

    // Исключаем carol; новые сообщения шифруются для оставшихся.
    match call(
        &server,
        &mut alice,
        Method::RemoveChatMember(proto::RemoveChatMemberRequest {
            chat_id: chat.chat_id.clone(),
            user_id: carol_user.clone(),
        }),
    )
    .await
    {
        R::RemoveChatMember(_) => {}
        other => panic!("remove failed: {other:?}"),
    }

    // Новое сообщение: доставка ключа только bob, чужие устройства не видны.
    expect_sent(
        send_message(
            &server,
            &mut alice,
            group_send_request(
                &chat.chat_id,
                "m2",
                vec![9; 32],
                "session-2",
                &[("b1", vec![0xBB; 16])],
            ),
        )
        .await,
    );
    // Копия для исключённого устройства отклоняется: оно больше не участник.
    let error = expect_error(
        send_message(
            &server,
            &mut alice,
            group_send_request(
                &chat.chat_id,
                "m3",
                vec![10; 32],
                "session-2",
                &[("c1", vec![0xCC; 16])],
            ),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);

    // Устройства carol недоступны, истории новых сообщений у неё нет.
    assert!(get_devices(&server, &mut alice, &[carol_user])
        .await
        .is_empty());
}

#[tokio::test]
async fn dev_group_plaintext_and_direct_stays_strict() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    register(&server, &mut alice, "alice", "a1", false).await;
    register(&server, &mut bob, "bob", "b1", false).await;
    let chat = create_group(
        &server,
        &mut alice,
        "Dev",
        &["bob"],
        proto::E2eMode::NoneDev,
    )
    .await;

    let sent = expect_sent(
        send_message(
            &server,
            &mut alice,
            proto::SendMessageRequest {
                chat_id: chat.chat_id.clone(),
                client_message_id: "m1".to_string(),
                content_kind: "text".to_string(),
                grouped_id: String::new(),
                reply_to_server_id: 0,
                plaintext_dev: "привет".to_string(),
                ..Default::default()
            },
        )
        .await,
    );
    assert_eq!(sent.plaintext_dev, "привет");
    let page = sync(&server, &mut bob, 0).await;
    let views = message_views(&page);
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].plaintext_dev, "привет");

    // Megolm-поля в dev-чате запрещены.
    let error = expect_error(
        send_message(
            &server,
            &mut alice,
            proto::SendMessageRequest {
                chat_id: chat.chat_id.clone(),
                client_message_id: "m2".to_string(),
                content_kind: "text".to_string(),
                grouped_id: String::new(),
                reply_to_server_id: 0,
                group_ciphertext: vec![1, 2, 3],
                group_session_id: "s".to_string(),
                ..Default::default()
            },
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);

    // Личный E2E-чат: group-поля и групповые ключи запрещены.
    let mut erin = ConnState::new(peer());
    let mut frank = ConnState::new(peer());
    register(&server, &mut erin, "erin", "e1", true).await;
    register(&server, &mut frank, "frank", "f1", true).await;
    let direct = create_direct_chat(&server, &mut erin, "frank", proto::E2eMode::OlmV1).await;
    let error = expect_error(
        send_message(
            &server,
            &mut erin,
            group_send_request(
                &direct.chat_id,
                "m1",
                vec![1; 32],
                "s",
                &[("f1", vec![1; 8])],
            ),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);

    // Групповой ключ в личном чате тоже запрещён, даже при полном покрытии.
    let error = expect_error(
        send_message(
            &server,
            &mut erin,
            proto::SendMessageRequest {
                chat_id: direct.chat_id,
                client_message_id: "m2".to_string(),
                content_kind: "text".to_string(),
                grouped_id: String::new(),
                reply_to_server_id: 0,
                recipients: vec![proto::RecipientPayload {
                    device_id: "f1".to_string(),
                    ciphertext: vec![1; 8],
                    e2e_version: rchat_proto::E2E_VERSION_OLM_V1,
                    olm_type: proto::OlmMessageType::Prekey as i32,
                    sender_curve25519: vec![0x22; 32],
                    payload_kind: proto::RecipientPayloadKind::GroupKey as i32,
                }],
                plaintext_dev: String::new(),
                group_ciphertext: Vec::new(),
                group_session_id: String::new(),
            },
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);
}

#[tokio::test]
async fn group_message_edit_and_delete() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    register(&server, &mut alice, "alice", "a1", true).await;
    register(&server, &mut bob, "bob", "b1", true).await;
    let chat = create_group(&server, &mut alice, "E2E", &["bob"], proto::E2eMode::OlmV1).await;
    let sent = expect_sent(
        send_message(
            &server,
            &mut alice,
            group_send_request(
                &chat.chat_id,
                "m1",
                vec![7; 32],
                "s1",
                &[("b1", vec![1; 16])],
            ),
        )
        .await,
    );

    // Правка без megolm-полей в E2E-группе запрещена.
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::EditMessage(proto::EditMessageRequest {
                server_id: sent.server_id,
                content_kind: "text".to_string(),
                ..Default::default()
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);

    // Правка: новая сессия и новый ciphertext (ротация по политике клиента).
    match call(
        &server,
        &mut alice,
        Method::EditMessage(proto::EditMessageRequest {
            server_id: sent.server_id,
            content_kind: "text".to_string(),
            recipients: vec![proto::RecipientPayload {
                device_id: "b1".to_string(),
                ciphertext: vec![2; 16],
                e2e_version: rchat_proto::E2E_VERSION_OLM_V1,
                olm_type: proto::OlmMessageType::Prekey as i32,
                sender_curve25519: vec![0x22; 32],
                payload_kind: proto::RecipientPayloadKind::GroupKey as i32,
            }],
            plaintext_dev: String::new(),
            group_ciphertext: vec![9; 32],
            group_session_id: "s2".to_string(),
        }),
    )
    .await
    {
        R::EditMessage(response) => {
            let view = response.message.expect("message");
            assert_eq!(view.group_ciphertext, vec![9; 32]);
            assert_eq!(view.group_session_id, "s2");
        }
        other => panic!("edit failed: {other:?}"),
    }

    // bob получает событие правки с новым ciphertext и копией ключа.
    let page = sync(&server, &mut bob, 0).await;
    let edited = page
        .events
        .iter()
        .find(|event| event.kind_hint == 12)
        .expect("edit event");
    match edited.payload.as_ref() {
        Some(Payload::MessageEdited(inner)) => {
            let view = inner.message.as_ref().expect("message");
            assert_eq!(view.group_ciphertext, vec![9; 32]);
            assert_eq!(view.group_session_id, "s2");
            assert_eq!(view.ciphertext, vec![2; 16]);
            assert_eq!(
                view.recipient_payload_kind,
                proto::RecipientPayloadKind::GroupKey as i32
            );
        }
        other => panic!("expected message_edited, got {other:?}"),
    }

    // Удаление: даже событие-надгробие не несёт megolm-ciphertext.
    assert!(matches!(
        call(
            &server,
            &mut alice,
            Method::DeleteMessage(proto::DeleteMessageRequest {
                server_id: sent.server_id,
            }),
        )
        .await,
        R::DeleteMessage(_)
    ));
    let page = sync(&server, &mut bob, page.up_to_seq).await;
    let deleted = page
        .events
        .iter()
        .find(|event| event.kind_hint == 13)
        .expect("delete event");
    match deleted.payload.as_ref() {
        Some(Payload::MessageDeleted(inner)) => {
            let view = inner.message.as_ref().expect("message");
            assert!(view.group_ciphertext.is_empty());
            assert!(view.group_session_id.is_empty());
            assert!(view.ciphertext.is_empty());
        }
        other => panic!("expected message_deleted, got {other:?}"),
    }

    // История отдаёт надгробие без содержимого.
    let messages = match call(
        &server,
        &mut bob,
        Method::GetHistory(proto::GetHistoryRequest {
            chat_id: chat.chat_id.clone(),
            before_server_id: 0,
            limit: 0,
        }),
    )
    .await
    {
        R::GetHistory(response) => response.messages,
        other => panic!("history failed: {other:?}"),
    };
    assert_eq!(messages.len(), 1);
    assert!(messages[0].group_ciphertext.is_empty());
    assert!(messages[0].plaintext_dev.is_empty());
    assert!(messages[0].deleted_at_ms > 0);
}

#[tokio::test]
async fn group_grants_device_visibility() {
    let server = test_server().await;
    let mut alice1 = ConnState::new(peer());
    let mut alice2 = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    let (alice_id, _) = register(&server, &mut alice1, "alice", "a1", true).await;
    login(&server, &mut alice2, "alice", "a2", true).await;
    register(&server, &mut bob, "bob", "b1", true).await;

    // Без общего чата чужие устройства не видны.
    assert!(
        get_devices(&server, &mut bob, std::slice::from_ref(&alice_id))
            .await
            .is_empty()
    );

    // Общий групповой чат открывает видимость устройств (нужно для
    // рассылки megolm-ключей всем устройствам участников).
    create_group(&server, &mut alice1, "G", &["bob"], proto::E2eMode::OlmV1).await;
    let devices = get_devices(&server, &mut bob, &[alice_id]).await;
    assert_eq!(devices.len(), 2);
    assert!(devices.iter().all(|d| d.e2e_version == 1));
}

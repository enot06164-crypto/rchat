//! Интеграционные тесты авторизации: обязательный вход, истечение и отзыв
//! сессий, жизненный цикл устройств, доступ к чужим чатам, атрибуция
//! отправителя по сессии и ограничения частоты.

mod common;

use common::*;

use rchat_proto::proto;
use rchat_proto::proto::request::Method;
use rchat_proto::proto::response::Result as R;
use rchat_server::service::ConnState;

/// Запрос Login с тестовым паролем.
fn login_request(username: &str, password: &str, device_id: &str) -> proto::LoginRequest {
    proto::LoginRequest {
        username: username.to_string(),
        password: password.to_string(),
        device: Some(device(device_id, false)),
        one_time_keys: Vec::new(),
        fallback_key: None,
    }
}

/// Запрос SendMessage для dev-чата.
fn plaintext_message(
    chat_id: &str,
    client_message_id: &str,
    text: &str,
) -> proto::SendMessageRequest {
    proto::SendMessageRequest {
        chat_id: chat_id.to_string(),
        client_message_id: client_message_id.to_string(),
        content_kind: "text".to_string(),
        reply_to_server_id: 0,
        recipients: Vec::new(),
        plaintext_dev: text.to_string(),
        ..Default::default()
    }
}

#[tokio::test]
async fn unauthenticated_requests_are_rejected() {
    let server = test_server().await;
    let mut state = ConnState::new(peer());
    let unknown_chat = "0".repeat(32);
    let methods = [
        Method::GetMe(proto::GetMeRequest {}),
        Method::ListChats(proto::ListChatsRequest {}),
        Method::Sync(proto::SyncRequest {
            after_seq: 0,
            limit: 0,
        }),
        Method::GetSnapshot(proto::SnapshotRequest {}),
        Method::GetHistory(proto::GetHistoryRequest {
            chat_id: unknown_chat.clone(),
            before_server_id: 0,
            limit: 10,
        }),
        Method::MarkReceipt(proto::MarkReceiptRequest {
            chat_id: unknown_chat,
            up_to_server_id: 1,
            read: true,
        }),
        Method::Logout(proto::LogoutRequest {}),
    ];
    for method in methods {
        let error = expect_error(call(&server, &mut state, method.clone()).await);
        assert_eq!(
            error.code,
            proto::ErrorCode::Unauthenticated as i32,
            "метод {method:?} должен требовать вход"
        );
    }
}

#[tokio::test]
async fn resume_rejects_unknown_token() {
    let server = test_server().await;
    let mut state = ConnState::new(peer());
    let error = expect_error(
        call(
            &server,
            &mut state,
            Method::Resume(proto::ResumeRequest {
                session_token: "не-существующий-токен".to_string(),
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::Unauthenticated as i32);
}

#[tokio::test]
async fn expired_session_loses_rights_immediately() {
    // Нулевой TTL: сессия истекает сразу после создания.
    let server = test_server_with(|config| config.session_ttl_seconds = 0).await;
    let mut alice = ConnState::new(peer());
    let (_user, token) = register(&server, &mut alice, "alice", "a1", false).await;

    // Даже живое соединение: следующий запрос отклонён и права сняты.
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::ListChats(proto::ListChatsRequest {}),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::SessionExpired as i32);
    assert!(alice.auth.is_none(), "права должны быть сняты");

    // Повторное подключение с тем же токеном тоже отклонено.
    let mut fresh = ConnState::new(peer());
    let error = expect_error(
        call(
            &server,
            &mut fresh,
            Method::Resume(proto::ResumeRequest {
                session_token: token,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::SessionExpired as i32);
}

#[tokio::test]
async fn logout_revokes_only_current_session() {
    let server = test_server().await;
    let mut b1 = ConnState::new(peer());
    let mut b2 = ConnState::new(peer());
    let (_user, token1) = register(&server, &mut b1, "bob", "b1", false).await;
    let (_user2, token2) = login(&server, &mut b2, "bob", "b2", false).await;

    match call(&server, &mut b1, Method::Logout(proto::LogoutRequest {})).await {
        R::Logout(_) => {}
        other => panic!("logout failed: {other:?}"),
    }

    // Токен завершённой сессии не восстанавливается.
    let mut fresh1 = ConnState::new(peer());
    let error = expect_error(
        call(
            &server,
            &mut fresh1,
            Method::Resume(proto::ResumeRequest {
                session_token: token1,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::SessionRevoked as i32);

    // Второе устройство продолжает работать.
    match call(
        &server,
        &mut b2,
        Method::ListChats(proto::ListChatsRequest {}),
    )
    .await
    {
        R::ListChats(_) => {}
        other => panic!("second session broken: {other:?}"),
    }
    let mut fresh2 = ConnState::new(peer());
    match call(
        &server,
        &mut fresh2,
        Method::Resume(proto::ResumeRequest {
            session_token: token2,
        }),
    )
    .await
    {
        R::Resume(_) => {}
        other => panic!("resume of second session failed: {other:?}"),
    }
}

#[tokio::test]
async fn revoked_device_sessions_die_and_relogin_is_refused() {
    let server = test_server().await;
    let mut b1 = ConnState::new(peer());
    let mut b2 = ConnState::new(peer());
    let (_user, token1) = register(&server, &mut b1, "bob", "b1", false).await;
    login(&server, &mut b2, "bob", "b2", false).await;

    // Второе устройство отзывает первое.
    match call(
        &server,
        &mut b2,
        Method::RevokeDevice(proto::RevokeDeviceRequest {
            device_id: "b1".to_string(),
        }),
    )
    .await
    {
        R::RevokeDevice(_) => {}
        other => panic!("revoke failed: {other:?}"),
    }

    // Токен отозванного устройства не восстанавливается.
    let mut fresh = ConnState::new(peer());
    let error = expect_error(
        call(
            &server,
            &mut fresh,
            Method::Resume(proto::ResumeRequest {
                session_token: token1,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::SessionRevoked as i32);

    // Список устройств показывает отзыв.
    let devices = match call(
        &server,
        &mut b2,
        Method::ListDevices(proto::ListDevicesRequest {}),
    )
    .await
    {
        R::ListDevices(response) => response.devices,
        other => panic!("list devices failed: {other:?}"),
    };
    let revoked = devices
        .iter()
        .find(|d| d.device_id == "b1")
        .expect("b1 в списке устройств");
    assert!(revoked.revoked_at_ms > 0, "устройство должно быть отозвано");

    // Повторный вход с отозванным device_id отклоняется.
    let mut again = ConnState::new(peer());
    let error = expect_error(
        call(
            &server,
            &mut again,
            Method::Login(login_request("bob", PASSWORD, "b1")),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::DeviceConflict as i32);

    // Второе устройство не затронуто.
    match call(
        &server,
        &mut b2,
        Method::ListChats(proto::ListChatsRequest {}),
    )
    .await
    {
        R::ListChats(_) => {}
        other => panic!("second device broken: {other:?}"),
    }
}

#[tokio::test]
async fn revoking_foreign_device_is_not_found() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    register(&server, &mut alice, "alice", "a1", false).await;
    register(&server, &mut bob, "bob", "b1", false).await;

    // Устройство ищется только среди устройств текущего пользователя.
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::RevokeDevice(proto::RevokeDeviceRequest {
                device_id: "b1".to_string(),
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::NotFound as i32);

    let devices = match call(
        &server,
        &mut bob,
        Method::ListDevices(proto::ListDevicesRequest {}),
    )
    .await
    {
        R::ListDevices(response) => response.devices,
        other => panic!("list devices failed: {other:?}"),
    };
    assert!(
        devices.iter().all(|d| d.revoked_at_ms == 0),
        "чужое устройство не должно быть отозвано"
    );
}

#[tokio::test]
async fn foreign_chat_is_inaccessible_and_sender_is_session_bound() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    let mut charlie = ConnState::new(peer());
    let (alice_id, _) = register(&server, &mut alice, "alice", "a1", false).await;
    let (bob_id, _) = register(&server, &mut bob, "bob", "b1", false).await;
    register(&server, &mut charlie, "charlie", "c1", false).await;

    let chat = create_direct_chat(&server, &mut alice, "bob", proto::E2eMode::NoneDev).await;
    let sent = expect_sent(
        send_message(
            &server,
            &mut alice,
            plaintext_message(&chat.chat_id, "m1", "секрет"),
        )
        .await,
    );
    // Отправитель берётся из аутентифицированной сессии.
    assert_eq!(sent.sender_user_id, alice_id);
    assert_eq!(sent.sender_device_id, "a1");

    // Чужие чаты не видны в списке.
    let chats = match call(
        &server,
        &mut charlie,
        Method::ListChats(proto::ListChatsRequest {}),
    )
    .await
    {
        R::ListChats(response) => response.chats,
        other => panic!("list chats failed: {other:?}"),
    };
    assert!(chats.is_empty(), "посторонний не должен видеть чужой чат");

    // Чтение, отправка, правка, удаление и receipts отклоняются.
    let attempts = [
        Method::GetHistory(proto::GetHistoryRequest {
            chat_id: chat.chat_id.clone(),
            before_server_id: 0,
            limit: 10,
        }),
        Method::SendMessage(plaintext_message(&chat.chat_id, "x1", "чужое")),
        Method::EditMessage(proto::EditMessageRequest {
            server_id: sent.server_id,
            content_kind: "text".to_string(),
            recipients: Vec::new(),
            plaintext_dev: "правка".to_string(),
            ..Default::default()
        }),
        Method::DeleteMessage(proto::DeleteMessageRequest {
            server_id: sent.server_id,
        }),
        Method::MarkReceipt(proto::MarkReceiptRequest {
            chat_id: chat.chat_id.clone(),
            up_to_server_id: sent.server_id,
            read: true,
        }),
    ];
    for method in attempts {
        let error = expect_error(call(&server, &mut charlie, method.clone()).await);
        assert_eq!(
            error.code,
            proto::ErrorCode::Forbidden as i32,
            "метод {method:?} должен быть запрещён для не-участника"
        );
    }

    // Участник, но не автор, не может править и удалять чужое сообщение.
    let error = expect_error(
        call(
            &server,
            &mut bob,
            Method::EditMessage(proto::EditMessageRequest {
                server_id: sent.server_id,
                content_kind: "text".to_string(),
                recipients: Vec::new(),
                plaintext_dev: "правка".to_string(),
                ..Default::default()
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::Forbidden as i32);
    let error = expect_error(
        call(
            &server,
            &mut bob,
            Method::DeleteMessage(proto::DeleteMessageRequest {
                server_id: sent.server_id,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::Forbidden as i32);

    // Получатель видит исходное сообщение, ничего не изменилось.
    let history = match call(
        &server,
        &mut bob,
        Method::GetHistory(proto::GetHistoryRequest {
            chat_id: chat.chat_id.clone(),
            before_server_id: 0,
            limit: 10,
        }),
    )
    .await
    {
        R::GetHistory(response) => response,
        other => panic!("history failed: {other:?}"),
    };
    assert_eq!(history.messages.len(), 1);
    assert_eq!(history.messages[0].sender_user_id, alice_id);
    assert_eq!(history.messages[0].sender_device_id, "a1");
    assert_eq!(history.messages[0].plaintext_dev, "секрет");
    assert_eq!(history.messages[0].deleted_at_ms, 0);

    // Ответ участника атрибутируется его сессией.
    let reply = expect_sent(
        send_message(
            &server,
            &mut bob,
            plaintext_message(&chat.chat_id, "m2", "ответ"),
        )
        .await,
    );
    assert_eq!(reply.sender_user_id, bob_id);
    assert_eq!(reply.sender_device_id, "b1");
}

#[tokio::test]
async fn register_rate_limit_is_enforced() {
    let server = test_server_with(|config| config.rate_limits.register_per_hour = 1).await;
    let mut alice = ConnState::new(peer());
    register(&server, &mut alice, "alice", "a1", false).await;

    // Вторая регистрация с того же сетевого ключа отклонена лимитом.
    let mut bob = ConnState::new(peer());
    let error = expect_error(
        call(
            &server,
            &mut bob,
            Method::Register(proto::RegisterRequest {
                username: "bob".to_string(),
                display_name: "bob display".to_string(),
                password: PASSWORD.to_string(),
                device: Some(device("b1", false)),
                one_time_keys: Vec::new(),
                fallback_key: None,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::RateLimited as i32);
    assert!(error.retryable);
}

#[tokio::test]
async fn login_rate_limit_counts_failures() {
    let server = test_server_with(|config| config.rate_limits.login_per_5min = 1).await;
    let mut alice = ConnState::new(peer());
    register(&server, &mut alice, "alice", "a1", false).await;

    // Первая попытка — неверный пароль, но она расходует окно.
    let mut bad = ConnState::new(peer());
    let error = expect_error(
        call(
            &server,
            &mut bad,
            Method::Login(login_request("alice", "неверный пароль", "a2")),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::InvalidCredentials as i32);

    // Вторая попытка с правильным паролем уже ограничена.
    let mut again = ConnState::new(peer());
    let error = expect_error(
        call(
            &server,
            &mut again,
            Method::Login(login_request("alice", PASSWORD, "a2")),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::RateLimited as i32);
}

#[tokio::test]
async fn send_rate_limit_is_enforced_per_user() {
    let server = test_server_with(|config| config.rate_limits.send_per_minute = 2).await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    register(&server, &mut alice, "alice", "a1", false).await;
    register(&server, &mut bob, "bob", "b1", false).await;
    let chat = create_direct_chat(&server, &mut alice, "bob", proto::E2eMode::NoneDev).await;

    for index in 0..2 {
        expect_sent(
            send_message(
                &server,
                &mut alice,
                plaintext_message(&chat.chat_id, &format!("m{index}"), "сообщение"),
            )
            .await,
        );
    }
    let error = expect_error(
        send_message(
            &server,
            &mut alice,
            plaintext_message(&chat.chat_id, "m2", "лишнее"),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::RateLimited as i32);

    // Лимит считается по пользователю: второй участник не затронут.
    expect_sent(
        send_message(
            &server,
            &mut bob,
            plaintext_message(&chat.chat_id, "b1", "ответ"),
        )
        .await,
    );
}

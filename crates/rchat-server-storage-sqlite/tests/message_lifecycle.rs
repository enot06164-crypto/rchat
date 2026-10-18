//! Этап 7: правка и удаление сообщений, receipts, а также догрузка этих
//! событий устройством, которое пропустило их в офлайне.
//!
//! Тесты идут через сервисный слой поверх SQLite без сети; сетевой транспорт
//! проверяется в `websocket_smoke.rs`, сквозные потоки — в `rchat-e2e-tests`
//! и CLI-smoke.

mod common;

use common::*;
use rchat_proto::proto;
use rchat_proto::proto::request::Method;
use rchat_proto::proto::response::Result as R;
use rchat_proto::proto::sync_event::Payload;
use rchat_server::service::{ConnState, Server};

fn send_request(
    chat_id: &str,
    client_message_id: &str,
    plaintext_dev: &str,
    recipients: Vec<proto::RecipientPayload>,
) -> proto::SendMessageRequest {
    proto::SendMessageRequest {
        chat_id: chat_id.to_string(),
        client_message_id: client_message_id.to_string(),
        content_kind: "text".to_string(),
        reply_to_server_id: 0,
        recipients,
        plaintext_dev: plaintext_dev.to_string(),
        ..Default::default()
    }
}

fn edit_request(
    server_id: u64,
    plaintext_dev: &str,
    recipients: Vec<proto::RecipientPayload>,
) -> proto::EditMessageRequest {
    proto::EditMessageRequest {
        server_id,
        content_kind: "text".to_string(),
        recipients,
        plaintext_dev: plaintext_dev.to_string(),
        ..Default::default()
    }
}

/// Ciphertext-копия для устройства. Содержимое сервер не проверяет — важно
/// лишь покрытие E2E-устройств.
fn recipient(device_id: &str, ciphertext: Vec<u8>) -> proto::RecipientPayload {
    proto::RecipientPayload {
        device_id: device_id.to_string(),
        ciphertext,
        e2e_version: rchat_proto::E2E_VERSION_OLM_V1,
        olm_type: proto::OlmMessageType::Normal as i32,
        sender_curve25519: vec![0x33; 32],
        ..Default::default()
    }
}

async fn history(server: &Server, state: &mut ConnState, chat_id: &str) -> Vec<proto::Message> {
    match call(
        server,
        state,
        Method::GetHistory(proto::GetHistoryRequest {
            chat_id: chat_id.to_string(),
            before_server_id: 0,
            limit: 50,
        }),
    )
    .await
    {
        R::GetHistory(response) => response.messages,
        other => panic!("history failed: {other:?}"),
    }
}

#[tokio::test]
async fn edit_requires_authorship_and_updates_content() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    let (alice_id, _) = register(&server, &mut alice, "alice", "a1", false).await;
    register(&server, &mut bob, "bob", "b1", false).await;
    let chat = create_direct_chat(&server, &mut alice, "bob", proto::E2eMode::NoneDev).await;

    let sent = expect_sent(
        send_message(
            &server,
            &mut alice,
            send_request(&chat.chat_id, "m1", "первый", Vec::new()),
        )
        .await,
    );

    // Правка не автором запрещена.
    let error = expect_error(
        call(
            &server,
            &mut bob,
            Method::EditMessage(edit_request(sent.server_id, "чужое", Vec::new())),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::Forbidden as i32);

    // Правка автором: edited_at заполнен, created_at не переписывается.
    let edited = match call(
        &server,
        &mut alice,
        Method::EditMessage(edit_request(sent.server_id, "второй", Vec::new())),
    )
    .await
    {
        R::EditMessage(response) => response.message.expect("edited message"),
        other => panic!("edit failed: {other:?}"),
    };
    assert!(edited.edited_at_ms > 0);
    assert_eq!(edited.created_at_ms, sent.created_at_ms);
    assert_eq!(edited.plaintext_dev, "второй");
    assert_eq!(edited.sender_user_id, alice_id);

    // История у собеседника отдаёт новый текст.
    let messages = history(&server, &mut bob, &chat.chat_id).await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].plaintext_dev, "второй");
    assert_eq!(messages[0].edited_at_ms, edited.edited_at_ms);

    // Журнал bob: chat_created, message, message_edited (с новым текстом).
    let page = sync(&server, &mut bob, 0).await;
    assert!(!page.resync_required);
    assert_eq!(page.events.len(), 3);
    let edited_event = page
        .events
        .iter()
        .find(|event| event.kind_hint == 12)
        .expect("edited event");
    match edited_event.payload.as_ref() {
        Some(Payload::MessageEdited(event)) => {
            let message = event.message.as_ref().expect("message payload");
            assert_eq!(message.server_id, sent.server_id);
            assert_eq!(message.plaintext_dev, "второй");
            assert_eq!(message.edited_at_ms, edited.edited_at_ms);
        }
        other => panic!("expected edited event, got {other:?}"),
    }

    // Правка несуществующего сообщения — not found.
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::EditMessage(edit_request(9_999, "нет", Vec::new())),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::NotFound as i32);
}

#[tokio::test]
async fn delete_is_author_only_idempotent_and_erases_content() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    register(&server, &mut alice, "alice", "a1", false).await;
    register(&server, &mut bob, "bob", "b1", false).await;
    let chat = create_direct_chat(&server, &mut alice, "bob", proto::E2eMode::NoneDev).await;

    let sent = expect_sent(
        send_message(
            &server,
            &mut alice,
            send_request(&chat.chat_id, "m1", "секрет", Vec::new()),
        )
        .await,
    );

    // Удалять чужие сообщения нельзя.
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

    // Автор удаляет: tombstones без содержимого.
    let deleted = match call(
        &server,
        &mut alice,
        Method::DeleteMessage(proto::DeleteMessageRequest {
            server_id: sent.server_id,
        }),
    )
    .await
    {
        R::DeleteMessage(response) => response.message.expect("deleted message"),
        other => panic!("delete failed: {other:?}"),
    };
    assert!(deleted.deleted_at_ms > 0);
    assert_eq!(deleted.plaintext_dev, "");

    // Повторное удаление идемпотентно.
    let again = match call(
        &server,
        &mut alice,
        Method::DeleteMessage(proto::DeleteMessageRequest {
            server_id: sent.server_id,
        }),
    )
    .await
    {
        R::DeleteMessage(response) => response.message.expect("deleted message"),
        other => panic!("repeated delete failed: {other:?}"),
    };
    assert_eq!(again.deleted_at_ms, deleted.deleted_at_ms);
    assert_eq!(again.plaintext_dev, "");

    // История: надгробие без текста.
    let messages = history(&server, &mut bob, &chat.chat_id).await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].deleted_at_ms, deleted.deleted_at_ms);
    assert_eq!(messages[0].plaintext_dev, "");

    // Журнал bob: событие удаления тоже без текста.
    let page = sync(&server, &mut bob, 0).await;
    let deleted_event = page
        .events
        .iter()
        .find(|event| event.kind_hint == 13)
        .expect("deleted event");
    match deleted_event.payload.as_ref() {
        Some(Payload::MessageDeleted(event)) => {
            let message = event.message.as_ref().expect("message payload");
            assert_eq!(message.server_id, sent.server_id);
            assert!(message.deleted_at_ms > 0);
            assert_eq!(message.plaintext_dev, "");
        }
        other => panic!("expected deleted event, got {other:?}"),
    }

    // Удалённое сообщение больше не редактируется.
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::EditMessage(edit_request(sent.server_id, "оживить", Vec::new())),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::Conflict as i32);
}

#[tokio::test]
async fn e2e_edit_requires_coverage_and_stores_only_ciphertext() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    register(&server, &mut alice, "alice", "a1", true).await;
    register(&server, &mut bob, "bob", "b1", true).await;
    let chat = create_direct_chat(&server, &mut alice, "bob", proto::E2eMode::OlmV1).await;

    let sent = expect_sent(
        send_message(
            &server,
            &mut alice,
            send_request(
                &chat.chat_id,
                "m1",
                "",
                vec![recipient("b1", vec![1, 2, 3])],
            ),
        )
        .await,
    );
    assert_eq!(sent.plaintext_dev, "");

    // Правка без покрытия E2E-устройств отклоняется: silent fallback в
    // plaintext исключён.
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::EditMessage(edit_request(sent.server_id, "", Vec::new())),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::E2eKeysMissing as i32);

    // Plaintext в E2E-чате запрещён и при правке.
    let error = expect_error(
        call(
            &server,
            &mut alice,
            Method::EditMessage(edit_request(
                sent.server_id,
                "утечка",
                vec![recipient("b1", vec![9])],
            )),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::E2eRequired as i32);

    // Корректная правка: новый ciphertext заменяет старый.
    let edited = match call(
        &server,
        &mut alice,
        Method::EditMessage(edit_request(
            sent.server_id,
            "",
            vec![recipient("b1", vec![9, 9, 9])],
        )),
    )
    .await
    {
        R::EditMessage(response) => response.message.expect("edited message"),
        other => panic!("edit failed: {other:?}"),
    };
    assert!(edited.edited_at_ms > 0);
    assert_eq!(edited.plaintext_dev, "");
    // Устройству редактора своя же копия не выдаётся.
    assert!(edited.ciphertext.is_empty());

    // Собеседник видит ровно новый шифротекст и ключ устройства-редактора.
    let messages = history(&server, &mut bob, &chat.chat_id).await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].ciphertext, vec![9, 9, 9]);
    assert_eq!(
        messages[0].sender_curve25519,
        vec![0x33; 32],
        "sender_curve25519 должен браться из записи получателя"
    );
    assert_eq!(messages[0].e2e_version, rchat_proto::E2E_VERSION_OLM_V1);
    assert_eq!(messages[0].edited_at_ms, edited.edited_at_ms);
    assert_eq!(messages[0].plaintext_dev, "");

    // Журнал bob: событие правки с новым ciphertext и без plaintext.
    let page = sync(&server, &mut bob, 0).await;
    let edited_event = page
        .events
        .iter()
        .find(|event| event.kind_hint == 12)
        .expect("edited event");
    match edited_event.payload.as_ref() {
        Some(Payload::MessageEdited(event)) => {
            let message = event.message.as_ref().expect("message payload");
            assert_eq!(message.ciphertext, vec![9, 9, 9]);
            assert_eq!(message.plaintext_dev, "");
        }
        other => panic!("expected edited event, got {other:?}"),
    }
}

#[tokio::test]
async fn receipts_require_membership_and_validate_range() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    let mut carol = ConnState::new(peer());
    register(&server, &mut alice, "alice", "a1", false).await;
    register(&server, &mut bob, "bob", "b1", false).await;
    register(&server, &mut carol, "carol", "c1", false).await;
    let chat = create_direct_chat(&server, &mut alice, "bob", proto::E2eMode::NoneDev).await;

    // Подтверждение «в будущее» — невалидно.
    let error = expect_error(
        call(
            &server,
            &mut bob,
            Method::MarkReceipt(proto::MarkReceiptRequest {
                chat_id: chat.chat_id.clone(),
                up_to_server_id: 1,
                read: false,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::InvalidRequest as i32);

    // Посторонний не может подтверждать в чужом чате.
    let error = expect_error(
        call(
            &server,
            &mut carol,
            Method::MarkReceipt(proto::MarkReceiptRequest {
                chat_id: chat.chat_id.clone(),
                up_to_server_id: 0,
                read: true,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::Forbidden as i32);

    let sent = expect_sent(
        send_message(
            &server,
            &mut alice,
            send_request(&chat.chat_id, "m1", "привет", Vec::new()),
        )
        .await,
    );

    // delivered → read: у автора оба события в журнале.
    for read in [false, true] {
        let result = call(
            &server,
            &mut bob,
            Method::MarkReceipt(proto::MarkReceiptRequest {
                chat_id: chat.chat_id.clone(),
                up_to_server_id: sent.server_id,
                read,
            }),
        )
        .await;
        assert!(matches!(result, R::MarkReceipt(_)));
    }
    let page = sync(&server, &mut alice, 0).await;
    let receipts: Vec<&proto::SyncEvent> = page
        .events
        .iter()
        .filter(|event| event.kind_hint == 14)
        .collect();
    assert_eq!(receipts.len(), 2);
    match receipts[0].payload.as_ref() {
        Some(Payload::Receipt(event)) => {
            assert_eq!(event.up_to_server_id, sent.server_id);
            assert!(!event.read);
        }
        other => panic!("expected receipt event, got {other:?}"),
    }
    match receipts[1].payload.as_ref() {
        Some(Payload::Receipt(event)) => {
            assert_eq!(event.up_to_server_id, sent.server_id);
            assert!(event.read);
        }
        other => panic!("expected receipt event, got {other:?}"),
    }

    // Подтверждение назад прогресс не откатывает и событий не плодит.
    let result = call(
        &server,
        &mut bob,
        Method::MarkReceipt(proto::MarkReceiptRequest {
            chat_id: chat.chat_id.clone(),
            up_to_server_id: 0,
            read: true,
        }),
    )
    .await;
    assert!(matches!(result, R::MarkReceipt(_)));
    let tail = sync(&server, &mut alice, page.up_to_seq).await;
    assert!(tail.events.is_empty());
}

#[tokio::test]
async fn foreign_user_cannot_edit_or_delete() {
    let server = test_server().await;
    let mut alice = ConnState::new(peer());
    let mut bob = ConnState::new(peer());
    let mut carol = ConnState::new(peer());
    register(&server, &mut alice, "alice", "a1", false).await;
    register(&server, &mut bob, "bob", "b1", false).await;
    register(&server, &mut carol, "carol", "c1", false).await;
    let chat = create_direct_chat(&server, &mut alice, "bob", proto::E2eMode::NoneDev).await;
    let sent = expect_sent(
        send_message(
            &server,
            &mut alice,
            send_request(&chat.chat_id, "m1", "привет", Vec::new()),
        )
        .await,
    );

    // Carol не состоит в чате: и правка, и удаление — forbidden.
    let error = expect_error(
        call(
            &server,
            &mut carol,
            Method::EditMessage(edit_request(sent.server_id, "чужое", Vec::new())),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::Forbidden as i32);
    let error = expect_error(
        call(
            &server,
            &mut carol,
            Method::DeleteMessage(proto::DeleteMessageRequest {
                server_id: sent.server_id,
            }),
        )
        .await,
    );
    assert_eq!(error.code, proto::ErrorCode::Forbidden as i32);

    // Сообщение не изменилось.
    let messages = history(&server, &mut bob, &chat.chat_id).await;
    assert_eq!(messages[0].plaintext_dev, "привет");
    assert_eq!(messages[0].edited_at_ms, 0);
    assert_eq!(messages[0].deleted_at_ms, 0);
}

/// Ключевой критерий этапа: устройство, пропустившее правку/удаление/receipt
/// в офлайне, догружает их из журнала при следующей синхронизации.
#[tokio::test]
async fn offline_device_catches_up_with_edits_deletes_and_receipts() {
    let server = test_server().await;
    let mut a1 = ConnState::new(peer());
    let mut a2 = ConnState::new(peer());
    let mut b1 = ConnState::new(peer());
    let (alice_id, _) = register(&server, &mut a1, "alice", "a1", false).await;
    // Второе устройство alice существует с самого начала, но «молчит» до
    // конца сценария — его журнал копится.
    let (a2_id, _) = login(&server, &mut a2, "alice", "a2", false).await;
    assert_eq!(a2_id, alice_id);
    let (bob_id, _) = register(&server, &mut b1, "bob", "b1", false).await;

    let chat = create_direct_chat(&server, &mut a1, "bob", proto::E2eMode::NoneDev).await;
    let m1 = expect_sent(
        send_message(
            &server,
            &mut a1,
            send_request(&chat.chat_id, "m1", "один", Vec::new()),
        )
        .await,
    );
    let m2 = expect_sent(
        send_message(
            &server,
            &mut a1,
            send_request(&chat.chat_id, "m2", "два", Vec::new()),
        )
        .await,
    );

    // Bob читает оба сообщения и подтверждает прочтение.
    let bob_page = sync(&server, &mut b1, 0).await;
    assert_eq!(bob_page.up_to_seq, 3);
    let result = call(
        &server,
        &mut b1,
        Method::MarkReceipt(proto::MarkReceiptRequest {
            chat_id: chat.chat_id.clone(),
            up_to_server_id: m2.server_id,
            read: true,
        }),
    )
    .await;
    assert!(matches!(result, R::MarkReceipt(_)));

    // Пока a2 «офлайн»: правка первого сообщения и удаление второго.
    let result = call(
        &server,
        &mut a1,
        Method::EditMessage(edit_request(m1.server_id, "один+", Vec::new())),
    )
    .await;
    assert!(matches!(result, R::EditMessage(_)));
    let result = call(
        &server,
        &mut a1,
        Method::DeleteMessage(proto::DeleteMessageRequest {
            server_id: m2.server_id,
        }),
    )
    .await;
    assert!(matches!(result, R::DeleteMessage(_)));

    // a2 синхронизируется впервые: получает всю историю событий по порядку.
    let page = sync(&server, &mut a2, 0).await;
    assert!(!page.resync_required);
    let kinds: Vec<u32> = page.events.iter().map(|event| event.kind_hint).collect();
    assert_eq!(kinds, vec![10, 11, 11, 14, 12, 13]);
    assert_eq!(page.up_to_seq, 6);

    match page.events[3].payload.as_ref() {
        Some(Payload::Receipt(event)) => {
            assert_eq!(event.user_id, bob_id);
            assert_eq!(event.up_to_server_id, m2.server_id);
            assert!(event.read);
        }
        other => panic!("expected receipt event, got {other:?}"),
    }
    match page.events[4].payload.as_ref() {
        Some(Payload::MessageEdited(event)) => {
            let message = event.message.as_ref().expect("message payload");
            assert_eq!(message.server_id, m1.server_id);
            assert_eq!(message.plaintext_dev, "один+");
        }
        other => panic!("expected edited event, got {other:?}"),
    }
    match page.events[5].payload.as_ref() {
        Some(Payload::MessageDeleted(event)) => {
            let message = event.message.as_ref().expect("message payload");
            assert_eq!(message.server_id, m2.server_id);
            assert!(message.deleted_at_ms > 0);
            assert_eq!(message.plaintext_dev, "");
        }
        other => panic!("expected deleted event, got {other:?}"),
    }

    // История на «догнавшем» устройстве отражает итоговое состояние.
    let messages = history(&server, &mut a2, &chat.chat_id).await;
    assert_eq!(messages.len(), 2);
    let edited = messages
        .iter()
        .find(|message| message.server_id == m1.server_id)
        .expect("m1");
    assert_eq!(edited.plaintext_dev, "один+");
    assert!(edited.edited_at_ms > 0);
    let deleted = messages
        .iter()
        .find(|message| message.server_id == m2.server_id)
        .expect("m2");
    assert!(deleted.deleted_at_ms > 0);
    assert_eq!(deleted.plaintext_dev, "");
}

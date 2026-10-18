//! Тесты SQLite-хранилища сервера: миграции, регистрация, сообщения с
//! событиями, идемпотентность, предключи и политика хранения журнала.

use rchat_server::model::*;
use rchat_server::repo::Repo;
use rchat_server_storage_sqlite::{migrate_to, schema_version, SqliteRepo, SCHEMA_VERSION};
use rusqlite::Connection;

fn new_user(name: &str) -> NewUser {
    NewUser {
        username: name.to_string(),
        display_name: name.to_string(),
        password_hash: "phc$test".to_string(),
        created_at_ms: 1_000,
    }
}

fn new_device(id: &str, keys: bool) -> NewDevice {
    NewDevice {
        device_id: id.to_string(),
        name: format!("device-{id}"),
        platform: "test".to_string(),
        e2e_version: if keys { 1 } else { 0 },
        ed25519: if keys { vec![7; 32] } else { Vec::new() },
        curve25519: if keys { vec![8; 32] } else { Vec::new() },
        one_time_keys: Vec::new(),
        fallback_key: None,
    }
}

fn new_session(token: &[u8]) -> NewSession {
    NewSession {
        token_hash: token.to_vec(),
        device_id: "d1".to_string(),
        created_at_ms: 1_000,
        expires_at_ms: 9_000_000,
    }
}

fn event(device_id: &str, payload: &[u8]) -> NewSyncEvent {
    NewSyncEvent {
        device_id: device_id.to_string(),
        kind_hint: 11,
        payload: payload.to_vec(),
        created_at_ms: 1_500,
    }
}

fn message(server_id: u64, chat_id: &str, device_id: &str, client_id: &str) -> MessageRecord {
    MessageRecord {
        server_id,
        chat_id: chat_id.to_string(),
        sender_user_id: "u1".to_string(),
        sender_device_id: device_id.to_string(),
        client_message_id: client_id.to_string(),
        content_kind: "text".to_string(),
        reply_to_server_id: 0,
        created_at_ms: 2_000,
        edited_at_ms: 0,
        deleted_at_ms: 0,
        sender_curve25519: Vec::new(),
        olm_type: 0,
        plaintext_dev: Some("hello".to_string()),
    }
}

fn chat(chat_id: &str) -> ChatRecord {
    ChatRecord {
        chat_id: chat_id.to_string(),
        kind: 1,
        e2e_mode: 1,
        created_at_ms: 1_200,
        updated_at_ms: 1_200,
        last_server_id: 0,
    }
}

fn member(chat_id: &str, user_id: &str) -> MemberRecord {
    MemberRecord {
        chat_id: chat_id.to_string(),
        user_id: user_id.to_string(),
        joined_at_ms: 1_200,
    }
}

#[tokio::test]
async fn migrations_fresh_and_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rchat.db");
    {
        let conn = Connection::open(&path).unwrap();
        migrate_to(&conn, SCHEMA_VERSION).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), SCHEMA_VERSION);
        // Повторный прогон не должен ничего ломать.
        migrate_to(&conn, SCHEMA_VERSION).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), SCHEMA_VERSION);
    }
    let repo = SqliteRepo::open(&path).unwrap();
    assert!(repo.find_user_by_id("missing").await.unwrap().is_none());
}

#[tokio::test]
async fn migration_upgrade_preserves_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rchat.db");
    {
        // Только первая версия схемы + данные.
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        migrate_to(&conn, 1).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), 1);
        conn.execute(
            "INSERT INTO users(user_id, username, display_name, password_hash, created_at_ms, updated_at_ms, disabled)
             VALUES('u-legacy', 'legacy', 'Legacy', 'phc$legacy', 10, 10, 0)",
            [],
        )
        .unwrap();
    }
    // Открытие догоняет схему до последней версии, данные остаются.
    let repo = SqliteRepo::open(&path).unwrap();
    let user = repo
        .find_user_by_id("u-legacy")
        .await
        .unwrap()
        .expect("legacy user preserved");
    assert_eq!(user.username, "legacy");
    let conn = Connection::open(&path).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), SCHEMA_VERSION);
}

#[tokio::test]
async fn register_creates_user_device_session() {
    let repo = SqliteRepo::open_in_memory().unwrap();
    let (user, session) = repo
        .create_user_with_device(
            &new_user("alice"),
            &new_device("d1", true),
            &new_session(b"token-1"),
        )
        .await
        .unwrap();
    assert_eq!(user.username, "alice");
    assert!(!user.user_id.is_empty());
    assert_eq!(session.user_id, user.user_id);

    let found = repo.find_user_by_username("alice").await.unwrap().unwrap();
    assert_eq!(found.user_id, user.user_id);
    let device = repo
        .find_device(&user.user_id, "d1")
        .await
        .unwrap()
        .expect("device created");
    assert!(device.has_e2e_keys());
    let by_token = repo
        .find_session_by_token_hash(b"token-1")
        .await
        .unwrap()
        .expect("session found");
    assert_eq!(by_token.session_id, session.session_id);

    // Повторная регистрация того же username — конфликт.
    let duplicate = repo
        .create_user_with_device(
            &new_user("alice"),
            &new_device("d2", false),
            &new_session(b"token-2"),
        )
        .await;
    assert!(matches!(
        duplicate,
        Err(rchat_server::repo::RepoError::Conflict(key)) if key == "username_taken"
    ));
}

#[tokio::test]
async fn direct_chat_unique_per_pair_and_mode() {
    let repo = SqliteRepo::open_in_memory().unwrap();
    let (alice, _) = repo
        .create_user_with_device(
            &new_user("alice"),
            &new_device("d1", true),
            &new_session(b"t1"),
        )
        .await
        .unwrap();
    let (bob, _) = repo
        .create_user_with_device(
            &new_user("bob"),
            &new_device("d2", true),
            &new_session(b"t2"),
        )
        .await
        .unwrap();

    repo.create_chat(
        &chat("c1"),
        &[member("c1", &alice.user_id), member("c1", &bob.user_id)],
        &[],
    )
    .await
    .unwrap();
    let found = repo
        .find_direct_chat(&bob.user_id, &alice.user_id)
        .await
        .unwrap()
        .expect("chat found regardless of order");
    assert_eq!(found.0.chat_id, "c1");

    let second = repo
        .create_chat(
            &chat("c2"),
            &[member("c2", &alice.user_id), member("c2", &bob.user_id)],
            &[],
        )
        .await;
    assert!(matches!(
        second,
        Err(rchat_server::repo::RepoError::Conflict(key)) if key == "chat_pair"
    ));
    let chats = repo.list_chats_for_user(&alice.user_id).await.unwrap();
    assert_eq!(chats.len(), 1);
    assert_eq!(chats[0].1.len(), 2);
}

#[tokio::test]
async fn message_insert_events_and_idempotency() {
    let repo = SqliteRepo::open_in_memory().unwrap();
    let chat_row = chat("c1");
    repo.create_chat(&chat_row, &[member("c1", "u1"), member("c1", "u2")], &[])
        .await
        .unwrap();

    let msg = message(1, "c1", "d1", "m-1");
    let recipients = vec![
        RecipientRecord {
            server_id: 1,
            device_id: "d2".to_string(),
            ciphertext: vec![1, 2, 3],
            e2e_version: 1,
            olm_type: 1,
            sender_curve25519: vec![9; 32],
        },
        RecipientRecord {
            server_id: 1,
            device_id: "d3".to_string(),
            ciphertext: vec![4, 5],
            e2e_version: 1,
            olm_type: 0,
            sender_curve25519: vec![9; 32],
        },
    ];
    let insert = NewMessage {
        message: msg.clone(),
        recipients: recipients.clone(),
        events: vec![
            event("d1", b"ev-1"),
            event("d2", b"ev-2"),
            event("d3", b"ev-3"),
        ],
    };
    let outcome = repo.insert_message(&insert).await.unwrap();
    match outcome {
        InsertMessageOutcome::Inserted {
            message,
            event_seqs,
        } => {
            assert_eq!(message.server_id, 1);
            assert_eq!(
                event_seqs,
                vec![
                    ("d1".to_string(), 1),
                    ("d2".to_string(), 1),
                    ("d3".to_string(), 1)
                ]
            );
        }
        other => panic!("expected insert, got {other:?}"),
    }
    // Повтор идемпотентен.
    let again = repo.insert_message(&insert).await.unwrap();
    assert!(matches!(again, InsertMessageOutcome::Duplicate(m) if m.server_id == 1));
    // Всего одно сообщение.
    let history = repo.list_history("c1", 0, 10).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(repo.max_message_server_id().await.unwrap(), 1);

    // Чат помнит последний server_id.
    let (chat_row, _) = repo.get_chat("c1").await.unwrap().unwrap();
    assert_eq!(chat_row.last_server_id, 1);

    // Страница журнала d2: событие одно, курсор двигается.
    let page = repo
        .fetch_sync_events("d2", 0, 10, 60_000, 500, 10_000)
        .await
        .unwrap();
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].seq, 1);
    assert_eq!(page.events[0].payload, b"ev-2");
    assert_eq!(page.up_to_seq, 1);
    assert!(!page.resync_required);

    let page2 = repo
        .fetch_sync_events("d2", 1, 10, 60_000, 500, 10_000)
        .await
        .unwrap();
    assert!(page2.events.is_empty());
    assert_eq!(page2.up_to_seq, 1);

    let recipient = repo.message_recipient(1, "d2").await.unwrap().unwrap();
    assert_eq!(recipient.ciphertext, vec![1, 2, 3]);
    let packed = repo.recipients_for_device(&[1], "d3").await.unwrap();
    assert_eq!(packed.len(), 1);
    assert_eq!(packed[0].ciphertext, vec![4, 5]);
}

#[tokio::test]
async fn message_edit_delete_and_events() {
    let repo = SqliteRepo::open_in_memory().unwrap();
    repo.create_chat(&chat("c1"), &[member("c1", "u1"), member("c1", "u2")], &[])
        .await
        .unwrap();
    let msg = message(1, "c1", "d1", "m-1");
    repo.insert_message(&NewMessage {
        message: msg,
        recipients: vec![],
        events: vec![event("d1", b"ev")],
    })
    .await
    .unwrap();

    let update = MessageUpdate {
        edited_at_ms: 3_000,
        deleted_at_ms: 0,
        content_kind: Some("text".to_string()),
        recipients: Some(vec![RecipientRecord {
            server_id: 1,
            device_id: "d9".to_string(),
            ciphertext: vec![7],
            e2e_version: 1,
            olm_type: 0,
            sender_curve25519: vec![1; 32],
        }]),
        events: vec![event("d1", b"edit")],
        editor_user_id: "u1".to_string(),
    };
    let (updated, seqs) = repo.update_message(1, &update).await.unwrap();
    assert_eq!(updated.edited_at_ms, 3_000);
    assert_eq!(seqs, vec![("d1".to_string(), 2)]);
    let recipient = repo.message_recipient(1, "d9").await.unwrap().unwrap();
    assert_eq!(recipient.ciphertext, vec![7]);

    // Чужой редактор — конфликт.
    let foreign = MessageUpdate {
        editor_user_id: "u2".to_string(),
        ..update.clone()
    };
    assert!(repo.update_message(1, &foreign).await.is_err());

    let delete = MessageUpdate {
        edited_at_ms: 0,
        deleted_at_ms: 4_000,
        content_kind: None,
        recipients: None,
        events: vec![event("d1", b"delete")],
        editor_user_id: "u1".to_string(),
    };
    let (deleted, _) = repo.update_message(1, &delete).await.unwrap();
    assert_eq!(deleted.deleted_at_ms, 4_000);
    // Редактировать удалённое нельзя.
    assert!(repo.update_message(1, &update).await.is_err());
}

#[tokio::test]
async fn bundles_consume_one_time_and_reuse_fallback() {
    let repo = SqliteRepo::open_in_memory().unwrap();
    let mut device = new_device("d1", true);
    device.one_time_keys = vec![
        PreKeyRecord {
            key_id: 1,
            public_key: vec![1; 32],
            fallback: false,
            created_at_ms: 5,
        },
        PreKeyRecord {
            key_id: 2,
            public_key: vec![2; 32],
            fallback: false,
            created_at_ms: 6,
        },
    ];
    device.fallback_key = Some(PreKeyRecord {
        key_id: 99,
        public_key: vec![9; 32],
        fallback: true,
        created_at_ms: 7,
    });
    let (user, _) = repo
        .create_user_with_device(&new_user("alice"), &device, &new_session(b"t1"))
        .await
        .unwrap();

    let first = repo
        .take_bundle(&user.user_id, "d1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.one_time_key.as_ref().unwrap().key_id, 1);
    assert!(!first.fallback);
    let second = repo
        .take_bundle(&user.user_id, "d1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.one_time_key.as_ref().unwrap().key_id, 2);
    assert!(!second.fallback);
    let third = repo
        .take_bundle(&user.user_id, "d1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(third.one_time_key.as_ref().unwrap().key_id, 99);
    assert!(third.fallback);
    // Резервный ключ не расходуется.
    let fourth = repo
        .take_bundle(&user.user_id, "d1")
        .await
        .unwrap()
        .unwrap();
    assert!(fourth.fallback);
    assert_eq!(fourth.one_time_key.as_ref().unwrap().key_id, 99);
}

#[tokio::test]
async fn sync_retention_prunes_and_requires_resync() {
    let repo = SqliteRepo::open_in_memory().unwrap();
    let events: Vec<NewSyncEvent> = (0..5)
        .map(|i| NewSyncEvent {
            device_id: "d1".to_string(),
            kind_hint: 11,
            payload: vec![i as u8],
            created_at_ms: 1_000 + i,
        })
        .collect();
    let seqs = repo.append_sync_events(&events).await.unwrap();
    assert_eq!(seqs.len(), 5);
    assert_eq!(seqs[4].1, 5);

    // Время «сейчас» далеко в будущем, храним минимум 2 события.
    let page = repo
        .fetch_sync_events("d1", 0, 10, 0, 2, 1_000_000)
        .await
        .unwrap();
    assert!(page.resync_required, "старые события удалены");
    assert_eq!(page.min_seq, 4);
    assert_eq!(page.up_to_seq, 5);
    assert_eq!(page.events.len(), 2);
    assert_eq!(page.events[0].seq, 4);

    // Курсор, идущий в ногу, получает события без resync.
    let page2 = repo
        .fetch_sync_events("d1", 3, 10, 0, 2, 1_000_000)
        .await
        .unwrap();
    assert!(!page2.resync_required);
    assert_eq!(page2.events.len(), 2);

    // Пагинация: лимит 1.
    let page3 = repo
        .fetch_sync_events("d1", 3, 1, 0, 2, 1_000_000)
        .await
        .unwrap();
    assert_eq!(page3.events.len(), 1);
    assert_eq!(page3.events[0].seq, 4);

    repo.record_cursor("d1", 4, 2_000).await.unwrap();
    repo.record_cursor("d1", 2, 2_001).await.unwrap();
    assert_eq!(repo.max_seq_for_device("d1").await.unwrap(), 5);
}

#[tokio::test]
async fn receipts_are_monotonic() {
    let repo = SqliteRepo::open_in_memory().unwrap();
    repo.create_chat(&chat("c1"), &[member("c1", "u1"), member("c1", "u2")], &[])
        .await
        .unwrap();
    let receipt = ReceiptRecord {
        chat_id: "c1".to_string(),
        user_id: "u1".to_string(),
        up_to_server_id: 5,
        read: false,
        updated_at_ms: 100,
    };
    let (update, seqs) = repo
        .upsert_receipt(&receipt, &[event("d2", b"receipt-1")])
        .await
        .unwrap();
    assert!(update.moved);
    assert_eq!(update.up_to_server_id, 5);
    assert_eq!(seqs.len(), 1);

    // Меньшее значение не откатывает receipt (и не создаёт событий),
    // прочтение поднимает флаг.
    let backward = ReceiptRecord {
        up_to_server_id: 3,
        read: true,
        updated_at_ms: 200,
        ..receipt
    };
    let (update, seqs) = repo
        .upsert_receipt(&backward, &[event("d2", b"receipt-2")])
        .await
        .unwrap();
    assert!(update.moved, "флаг прочтения изменился");
    assert_eq!(update.up_to_server_id, 5);
    assert!(update.read);
    assert_eq!(seqs.len(), 1);

    let (update, seqs) = repo
        .upsert_receipt(&backward, &[event("d2", b"receipt-3")])
        .await
        .unwrap();
    assert!(!update.moved);
    assert!(seqs.is_empty());
}

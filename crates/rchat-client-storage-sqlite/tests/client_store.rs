//! Проверки клиентского SQLite-хранилища: миграции, чаты, сообщения,
//! outbox, синхронизация и состояние криптографии.

use rchat_client_storage_sqlite::{ClientSqliteStorage, SCHEMA_VERSION};
use rchat_core::traits::{CryptoStateStore, Storage};
use rchat_core::types::{
    ChatRecord, DeviceRecord, LocalAccount, MemberRecord, MessageRecord, MessageStatus,
    OutboxRecord, OutboxState, ReceiptRecord, SyncApply, UserRecord,
};
use rchat_proto::proto::{ChatKind, E2eMode};

fn storage() -> ClientSqliteStorage {
    ClientSqliteStorage::open_in_memory().expect("open in memory")
}

fn account() -> LocalAccount {
    LocalAccount {
        user_id: "u_alice".to_string(),
        username: "alice".to_string(),
        display_name: "Alice".to_string(),
        device_id: "d_alice".to_string(),
    }
}

fn chat(id: &str, updated_at: u64) -> ChatRecord {
    ChatRecord {
        cloud_key: Vec::new(),
        chat_id: id.to_string(),
        kind: ChatKind::Direct,
        e2e_mode: E2eMode::NoneDev,
        title: "Bob".to_string(),
        peer_user_id: Some("u_bob".to_string()),
        created_at_ms: 1_000,
        updated_at_ms: updated_at,
        last_server_id: 0,
    }
}

fn member(user_id: &str) -> MemberRecord {
    MemberRecord {
        user_id: user_id.to_string(),
        username: user_id.trim_start_matches("u_").to_string(),
        display_name: user_id.to_string(),
        role: rchat_proto::proto::ChatMemberRole::Member,
        joined_at_ms: 1_000,
    }
}

fn outgoing_message(local_id: &str, chat_id: &str) -> MessageRecord {
    MessageRecord {
        local_id: local_id.to_string(),
        server_id: None,
        chat_id: chat_id.to_string(),
        sender_user_id: "u_alice".to_string(),
        sender_device_id: "d_alice".to_string(),
        content_kind: "text".to_string(),
        grouped_id: None,
        attachment: None,
        body: Some(format!("привет {local_id}")),
        reply_to_server_id: None,
        status: Some(MessageStatus::Pending),
        created_at_ms: 2_000,
        edited_at_ms: None,
        deleted_at_ms: None,
    }
}

fn outbox_record(local_id: &str, chat_id: &str) -> OutboxRecord {
    OutboxRecord {
        local_id: local_id.to_string(),
        chat_id: chat_id.to_string(),
        idempotency_key: local_id.to_string(),
        request: vec![1, 2, 3],
        state: OutboxState::Pending,
        attempts: 0,
        next_attempt_at_ms: 0,
        last_error_code: None,
        last_error: None,
        created_at_ms: 2_000,
        server_message_id: None,
    }
}

fn incoming_message(server_id: u64, chat_id: &str) -> MessageRecord {
    MessageRecord {
        local_id: format!("s:{server_id}"),
        server_id: Some(server_id),
        chat_id: chat_id.to_string(),
        sender_user_id: "u_bob".to_string(),
        sender_device_id: "d_bob".to_string(),
        content_kind: "text".to_string(),
        grouped_id: None,
        attachment: None,
        body: Some(format!("ответ {server_id}")),
        reply_to_server_id: None,
        status: None,
        created_at_ms: 3_000 + server_id,
        edited_at_ms: None,
        deleted_at_ms: None,
    }
}

#[tokio::test]
async fn migrations_are_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("client.sqlite3");
    {
        let store = ClientSqliteStorage::open(&path).expect("open");
        assert_eq!(store.schema_version().expect("version"), SCHEMA_VERSION);
        store.migrate().await.expect("migrate");
        store.migrate().await.expect("migrate again");
        assert_eq!(store.schema_version().expect("version"), SCHEMA_VERSION);
    }
    // Повторное открытие уже мигрированной БД не меняет версию.
    let store = ClientSqliteStorage::open(&path).expect("reopen");
    assert_eq!(store.schema_version().expect("version"), SCHEMA_VERSION);
}

#[tokio::test]
async fn account_roundtrip_and_clear_all() {
    let store = storage();
    store.save_account(&account()).await.expect("save");
    let loaded = store.load_account().await.expect("load").expect("account");
    assert_eq!(loaded, account());

    store
        .upsert_chat(&chat("c1", 5), &[member("u_alice"), member("u_bob")])
        .await
        .expect("chat");
    store
        .insert_outgoing(&outgoing_message("m1", "c1"), &outbox_record("m1", "c1"))
        .await
        .expect("outbox");
    store
        .crypto_save("olm_account", b"secret")
        .await
        .expect("crypto");

    store.clear_all().await.expect("clear");
    assert!(store.load_account().await.expect("load").is_none());
    assert!(store.list_chats().await.expect("chats").is_empty());
    assert!(store.get_message("m1").await.expect("message").is_none());
    assert!(store.get_outbox("m1").await.expect("outbox").is_none());
    assert!(store.get_sync_cursor().await.expect("cursor") == 0);
    assert!(store
        .crypto_load("olm_account")
        .await
        .expect("crypto")
        .is_none());
}

#[tokio::test]
async fn chats_and_members_are_upserted() {
    let store = storage();
    store
        .upsert_chat(&chat("c1", 5), &[member("u_alice"), member("u_bob")])
        .await
        .expect("chat");
    let loaded = store.get_chat("c1").await.expect("get").expect("chat");
    assert_eq!(loaded.title, "Bob");
    assert_eq!(loaded.peer_user_id.as_deref(), Some("u_bob"));
    assert_eq!(store.chat_members("c1").await.expect("members").len(), 2);

    // Обновление заголовка и полная замена участников.
    let mut updated = chat("c1", 10);
    updated.title = "Robert".to_string();
    store
        .upsert_chat(&updated, &[member("u_alice")])
        .await
        .expect("update");
    assert_eq!(store.chat_members("c1").await.expect("members").len(), 1);
    let loaded = store.get_chat("c1").await.expect("get").expect("chat");
    assert_eq!(loaded.title, "Robert");
    assert_eq!(loaded.updated_at_ms, 10);

    // Snapshot заменяет список чатов целиком, сообщения не затрагиваются.
    store
        .insert_outgoing(&outgoing_message("m1", "c1"), &outbox_record("m1", "c1"))
        .await
        .expect("outbox");
    store
        .replace_chats(&[(chat("c2", 20), vec![member("u_alice")])])
        .await
        .expect("replace");
    assert!(store.get_chat("c1").await.expect("get").is_none());
    assert!(store.get_chat("c2").await.expect("get").is_some());
    assert!(store.get_message("m1").await.expect("message").is_some());
}

#[tokio::test]
async fn messages_are_listed_newest_first_with_pagination() {
    let store = storage();
    store
        .upsert_chat(&chat("c1", 5), &[member("u_alice")])
        .await
        .expect("chat");
    for server_id in 1..=3 {
        store
            .apply_sync_batch(
                &[SyncApply::InsertMessage(incoming_message(server_id, "c1"))],
                server_id,
            )
            .await
            .expect("insert");
    }
    // Локальное сообщение ещё без server_id — считается самым новым.
    store
        .insert_outgoing(
            &outgoing_message("m_pending", "c1"),
            &outbox_record("m_pending", "c1"),
        )
        .await
        .expect("outbox");

    let all = store.list_messages("c1", None, 10).await.expect("list");
    assert_eq!(all.len(), 4);
    assert_eq!(all[0].local_id, "m_pending");
    assert_eq!(all[1].server_id, Some(3));
    assert_eq!(all[3].server_id, Some(1));

    let page = store.list_messages("c1", Some(3), 2).await.expect("page");
    assert_eq!(page.len(), 2);
    assert_eq!(page[0].server_id, Some(2));
    assert_eq!(page[1].server_id, Some(1));

    let by_server = store
        .get_message_by_server(2)
        .await
        .expect("get")
        .expect("message");
    assert_eq!(by_server.local_id, "s:2");
}

#[tokio::test]
async fn outbox_lifecycle_ack_and_failure() {
    let store = storage();
    store
        .upsert_chat(&chat("c1", 5), &[member("u_alice")])
        .await
        .expect("chat");
    store
        .insert_outgoing(&outgoing_message("m1", "c1"), &outbox_record("m1", "c1"))
        .await
        .expect("outbox");

    let due = store.due_outbox(0, 10).await.expect("due");
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].local_id, "m1");
    assert_eq!(due[0].state, OutboxState::Pending);

    // Промежуточная ошибка: статус, счётчик попыток и время следующей попытки.
    store
        .apply_outbox_failure("m1", OutboxState::Pending, Some(1401), "busy", 5_000)
        .await
        .expect("failure");
    let record = store.get_outbox("m1").await.expect("get").expect("outbox");
    assert_eq!(record.attempts, 1);
    assert_eq!(record.next_attempt_at_ms, 5_000);
    assert_eq!(record.last_error.as_deref(), Some("busy"));
    let message = store
        .get_message("m1")
        .await
        .expect("get")
        .expect("message");
    assert_eq!(message.status, Some(MessageStatus::Pending));
    assert!(store.due_outbox(4_999, 10).await.expect("due").is_empty());

    // Разрыв соединения: Sending → Pending.
    store
        .apply_outbox_failure("m1", OutboxState::Sending, None, "lost", 0)
        .await
        .expect("sending");
    store.reset_inflight_outbox().await.expect("reset");
    let record = store.get_outbox("m1").await.expect("get").expect("outbox");
    assert_eq!(record.state, OutboxState::Pending);
    let message = store
        .get_message("m1")
        .await
        .expect("get")
        .expect("message");
    assert_eq!(message.status, Some(MessageStatus::Pending));

    // Повтор упавшего сообщения: та же запись возвращается в очередь (новая
    // копия не создаётся), счётчик попыток сброшен.
    store
        .apply_outbox_failure("m1", OutboxState::Failed, Some(1403), "forbidden", 0)
        .await
        .expect("failed");
    assert!(store.requeue_outbox("m1").await.expect("requeue"));
    // Повтор уже возвращённой в очередь записи ничего не меняет.
    assert!(!store.requeue_outbox("m1").await.expect("requeue"));
    let record = store.get_outbox("m1").await.expect("get").expect("outbox");
    assert_eq!(record.state, OutboxState::Pending);
    assert_eq!(record.attempts, 0);
    assert!(record.last_error.is_none());
    let message = store
        .get_message("m1")
        .await
        .expect("get")
        .expect("message");
    assert_eq!(message.status, Some(MessageStatus::Sending));
    // Повтор уже отправленного не меняет ничего.
    store.apply_send_ack("m1", 42).await.expect("ack");
    assert!(!store.requeue_outbox("m1").await.expect("requeue"));

    // Успешное подтверждение.
    store.apply_send_ack("m1", 42).await.expect("ack");
    assert!(store.get_outbox("m1").await.expect("get").is_none());
    let message = store
        .get_message("m1")
        .await
        .expect("get")
        .expect("message");
    assert_eq!(message.server_id, Some(42));
    assert_eq!(message.status, Some(MessageStatus::Sent));
    let chat = store.get_chat("c1").await.expect("get").expect("chat");
    assert_eq!(chat.last_server_id, 42);
    // Повторное подтверждение идемпотентно.
    store.apply_send_ack("m1", 42).await.expect("ack again");
}

#[tokio::test]
async fn outbox_stats_counts_total_and_due() {
    let store = storage();
    store
        .upsert_chat(&chat("c1", 5), &[member("u_alice")])
        .await
        .expect("chat");
    assert_eq!(store.outbox_stats(0).await.expect("stats"), (0, 0));

    store
        .insert_outgoing(&outgoing_message("m1", "c1"), &outbox_record("m1", "c1"))
        .await
        .expect("outbox");
    let mut delayed = outbox_record("m2", "c1");
    delayed.next_attempt_at_ms = 10_000;
    store
        .insert_outgoing(&outgoing_message("m2", "c1"), &delayed)
        .await
        .expect("outbox");

    assert_eq!(store.outbox_stats(5_000).await.expect("stats"), (2, 1));
    assert_eq!(store.outbox_stats(20_000).await.expect("stats"), (2, 2));

    store.apply_send_ack("m1", 7).await.expect("ack");
    assert_eq!(store.outbox_stats(20_000).await.expect("stats"), (1, 1));
}

#[tokio::test]
async fn send_ack_removes_echo_duplicate() {
    let store = storage();
    store
        .upsert_chat(&chat("c1", 5), &[member("u_alice")])
        .await
        .expect("chat");
    store
        .insert_outgoing(
            &outgoing_message("local_1", "c1"),
            &outbox_record("local_1", "c1"),
        )
        .await
        .expect("outbox");

    // Собственное сообщение пришло синхронизацией до подтверждения отправки.
    let mut echo = incoming_message(7, "c1");
    echo.sender_user_id = "u_alice".to_string();
    store
        .apply_sync_batch(&[SyncApply::InsertMessage(echo)], 1)
        .await
        .expect("sync");
    assert_eq!(
        store
            .list_messages("c1", None, 10)
            .await
            .expect("list")
            .len(),
        2
    );

    store.apply_send_ack("local_1", 7).await.expect("ack");
    let messages = store.list_messages("c1", None, 10).await.expect("list");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].local_id, "local_1");
    assert_eq!(messages[0].status, Some(MessageStatus::Sent));
    let chat = store.get_chat("c1").await.expect("get").expect("chat");
    assert_eq!(chat.last_server_id, 7);
}

#[tokio::test]
async fn sync_batch_applies_edits_receipts_and_device_changes() {
    let store = storage();
    let batch = vec![
        SyncApply::UpsertChat {
            chat: chat("c1", 5),
            members: vec![member("u_alice"), member("u_bob")],
        },
        SyncApply::InsertMessage(incoming_message(1, "c1")),
        SyncApply::EditMessage {
            server_id: 1,
            content_kind: "text".to_string(),
            body: Some("исправлено".to_string()),
            attachment: None,
            edited_at_ms: 9_000,
        },
        SyncApply::UpsertReceipt(ReceiptRecord {
            chat_id: "c1".to_string(),
            user_id: "u_bob".to_string(),
            delivered_up_to: 5,
            read_up_to: 3,
            updated_at_ms: 9_000,
        }),
        SyncApply::UpsertUser(UserRecord {
            user_id: "u_bob".to_string(),
            username: "bob".to_string(),
            display_name: "Bob".to_string(),
        }),
        SyncApply::UpsertDevice(DeviceRecord {
            device_id: "d_bob".to_string(),
            user_id: "u_bob".to_string(),
            name: "phone".to_string(),
            platform: "linux".to_string(),
            e2e_version: 1,
            ed25519: vec![1; 32],
            curve25519: vec![2; 32],
            revoked_at_ms: None,
        }),
        SyncApply::DeviceRevoked {
            device_id: "d_bob".to_string(),
            revoked_at_ms: 10_000,
        },
        SyncApply::Skip {
            seq: 7,
            kind_hint: 99,
        },
    ];
    store.apply_sync_batch(&batch, 7).await.expect("batch");
    assert_eq!(store.get_sync_cursor().await.expect("cursor"), 7);

    let message = store
        .get_message_by_server(1)
        .await
        .expect("get")
        .expect("message");
    assert_eq!(message.body.as_deref(), Some("исправлено"));
    assert_eq!(message.edited_at_ms, Some(9_000));

    let receipts = store.get_receipts("c1").await.expect("receipts");
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].delivered_up_to, 5);

    let device = store
        .get_device("d_bob")
        .await
        .expect("get")
        .expect("device");
    assert_eq!(device.revoked_at_ms, Some(10_000));

    // Повторное применение той же пачки не меняет состояние.
    store
        .apply_sync_batch(&batch, 7)
        .await
        .expect("batch again");
    let messages = store.list_messages("c1", None, 10).await.expect("list");
    assert_eq!(messages.len(), 1);
    assert_eq!(store.get_receipts("c1").await.expect("receipts").len(), 1);

    // Монотонность receipts: меньшие значения не откатывают прогресс.
    store
        .apply_sync_batch(
            &[SyncApply::UpsertReceipt(ReceiptRecord {
                chat_id: "c1".to_string(),
                user_id: "u_bob".to_string(),
                delivered_up_to: 2,
                read_up_to: 1,
                updated_at_ms: 1,
            })],
            8,
        )
        .await
        .expect("receipt");
    let receipts = store.get_receipts("c1").await.expect("receipts");
    assert_eq!(receipts[0].delivered_up_to, 5);
    assert_eq!(receipts[0].read_up_to, 3);

    // Удаление сообщения.
    store
        .apply_sync_batch(
            &[SyncApply::DeleteMessage {
                server_id: 1,
                deleted_at_ms: 11_000,
            }],
            9,
        )
        .await
        .expect("delete");
    let message = store
        .get_message_by_server(1)
        .await
        .expect("get")
        .expect("message");
    assert_eq!(message.deleted_at_ms, Some(11_000));
}

#[tokio::test]
async fn sync_cursor_only_grows() {
    let store = storage();
    store.apply_sync_batch(&[], 5).await.expect("batch");
    store.apply_sync_batch(&[], 3).await.expect("batch");
    assert_eq!(store.get_sync_cursor().await.expect("cursor"), 5);
    store.set_sync_cursor(1).await.expect("set");
    assert_eq!(store.get_sync_cursor().await.expect("cursor"), 1);
}

#[tokio::test]
async fn users_and_devices_cache() {
    let store = storage();
    store
        .upsert_user(&UserRecord {
            user_id: "u_bob".to_string(),
            username: "bob".to_string(),
            display_name: "Bob".to_string(),
        })
        .await
        .expect("user");
    let user = store.get_user("u_bob").await.expect("get").expect("user");
    assert_eq!(user.username, "bob");
    assert_eq!(store.list_users().await.expect("list").len(), 1);

    let device = DeviceRecord {
        device_id: "d_bob".to_string(),
        user_id: "u_bob".to_string(),
        name: "phone".to_string(),
        platform: "linux".to_string(),
        e2e_version: 1,
        ed25519: vec![1; 32],
        curve25519: vec![2; 32],
        revoked_at_ms: None,
    };
    store.upsert_device(&device).await.expect("device");
    assert_eq!(store.list_devices_of("u_bob").await.expect("list").len(), 1);

    // Пустые ключи не затирают уже закешированные.
    let mut empty_keys = device.clone();
    empty_keys.ed25519 = Vec::new();
    empty_keys.curve25519 = Vec::new();
    store.upsert_device(&empty_keys).await.expect("device");
    let cached = store
        .get_device("d_bob")
        .await
        .expect("get")
        .expect("device");
    assert_eq!(cached.ed25519.len(), 32);

    // Отзыв устройства монотонен.
    store
        .apply_sync_batch(
            &[SyncApply::DeviceRevoked {
                device_id: "d_bob".to_string(),
                revoked_at_ms: 100,
            }],
            1,
        )
        .await
        .expect("revoke");
    let mut fresh = device.clone();
    fresh.revoked_at_ms = None;
    store.upsert_device(&fresh).await.expect("device");
    let cached = store
        .get_device("d_bob")
        .await
        .expect("get")
        .expect("device");
    assert_eq!(cached.revoked_at_ms, Some(100));

    store.delete_device("d_bob").await.expect("delete");
    assert!(store.get_device("d_bob").await.expect("get").is_none());
}

#[tokio::test]
async fn crypto_state_crud_and_prefix_delete() {
    let store = storage();
    store
        .crypto_save("olm_account", b"acc")
        .await
        .expect("save");
    store
        .crypto_save("olm_session:d1", b"session1")
        .await
        .expect("save");
    store
        .crypto_save("olm_session:d2", b"session2")
        .await
        .expect("save");
    assert_eq!(
        store.crypto_load("olm_session:d1").await.expect("load"),
        Some(b"session1".to_vec())
    );
    // OVERWRITE.
    store
        .crypto_save("olm_account", b"acc2")
        .await
        .expect("save");
    assert_eq!(
        store.crypto_load("olm_account").await.expect("load"),
        Some(b"acc2".to_vec())
    );

    let deleted = store
        .crypto_delete_prefix("olm_session:")
        .await
        .expect("prefix");
    assert_eq!(deleted, 2);
    assert!(store
        .crypto_load("olm_session:d1")
        .await
        .expect("load")
        .is_none());
    assert!(store
        .crypto_load("olm_account")
        .await
        .expect("load")
        .is_some());

    store.crypto_delete("olm_account").await.expect("delete");
    assert!(store
        .crypto_load("olm_account")
        .await
        .expect("load")
        .is_none());
    // Удаление отсутствующего ключа — не ошибка.
    store
        .crypto_delete("olm_account")
        .await
        .expect("delete again");
    assert_eq!(store.crypto_delete_prefix("нет:").await.expect("prefix"), 0);
}

/// Миграция v2: роли участников и учёт выданных ключей групповых сессий.
#[tokio::test]
async fn migration_adds_member_roles_and_group_key_shares() {
    let store = storage();
    assert_eq!(
        store.schema_version().expect("version"),
        SCHEMA_VERSION,
        "схема должна подниматься до актуальной версии"
    );

    let mut group = chat("c_group", 1_000);
    group.kind = ChatKind::Group;
    group.e2e_mode = E2eMode::OlmV1;
    group.title = "Проект".to_string();
    group.peer_user_id = None;
    let mut owner = member("u_alice");
    owner.role = rchat_proto::proto::ChatMemberRole::Owner;
    store
        .upsert_chat(&group, &[owner, member("u_bob")])
        .await
        .expect("upsert group");

    let members = store.chat_members("c_group").await.expect("members");
    assert_eq!(members.len(), 2);
    let owner = members
        .iter()
        .find(|m| m.user_id == "u_alice")
        .expect("owner");
    assert_eq!(owner.role, rchat_proto::proto::ChatMemberRole::Owner);
    let bob = members.iter().find(|m| m.user_id == "u_bob").expect("bob");
    assert_eq!(bob.role, rchat_proto::proto::ChatMemberRole::Member);

    // Учёт выдачи ключей: идемпотентное добавление и очистка по чату.
    assert!(store
        .group_key_shares("c_group", "s1")
        .await
        .expect("empty")
        .is_empty());
    store
        .add_group_key_shares(
            "c_group",
            "s1",
            &["d_bob".to_string(), "d_carol".to_string()],
        )
        .await
        .expect("add");
    store
        .add_group_key_shares("c_group", "s1", &["d_carol".to_string()])
        .await
        .expect("add again");
    let shares = store
        .group_key_shares("c_group", "s1")
        .await
        .expect("shares");
    assert_eq!(shares, vec!["d_bob".to_string(), "d_carol".to_string()]);
    // Записи другого чата не затрагиваются.
    store
        .add_group_key_shares("c_other", "s1", &["d_x".to_string()])
        .await
        .expect("add other");
    store
        .clear_group_key_shares("c_group")
        .await
        .expect("clear");
    assert!(store
        .group_key_shares("c_group", "s1")
        .await
        .expect("cleared")
        .is_empty());
    assert_eq!(
        store
            .group_key_shares("c_other", "s1")
            .await
            .expect("other"),
        vec!["d_x".to_string()]
    );

    // clear_all стирает и групповые записи.
    store.clear_all().await.expect("clear all");
    assert!(store
        .group_key_shares("c_other", "s1")
        .await
        .expect("after clear")
        .is_empty());
}

//! Этап 7 на уровне клиентского ядра: правка и удаление E2E-сообщений,
//! receipts и догрузка изменений на второе устройство автора.
//!
//! Проверяемые инварианты:
//! - правку может выполнить любое устройство автора; новый текст шифруется
//!   заново для всех устройств, сервер видит только новый шифротекст;
//! - получатель расшифровывает правку с другого устройства автора
//!   (identity-ключ берётся из записи устройства-редактора, а не из
//!   исходного отправителя);
//! - удаление для всех стирает ciphertext на сервере и текст в локальном
//!   кеше каждого устройства;
//! - read receipt доезжает до всех устройств автора через журнал.
//!
//! Общий харнесс — в `tests/common/mod.rs`.

mod common;

use common::*;
use rchat_core::types::ChatMode;
use rchat_proto::proto;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_edit_delete_and_receipts_reach_all_devices() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let alice1_dir = root.path().join("alice1");
    let alice2_dir = root.path().join("alice2");
    let bob_dir = root.path().join("bob");

    // alice1 (автор), alice2 — её второе устройство, bob — получатель.
    let (alice1, alice1_storage) = build_client(&alice1_dir, harness.port(), "alice-dev1");
    alice1.start().await.expect("start alice1");
    register_user(&alice1, "alice", "Alice", PASSWORD_ALICE).await;

    let (alice2, _alice2_storage) = build_client(&alice2_dir, harness.port(), "alice-dev2");
    alice2.start().await.expect("start alice2");
    login_user(&alice2, "alice", PASSWORD_ALICE).await;

    let (bob, _bob_storage) = build_client(&bob_dir, harness.port(), "bob-dev");
    bob.start().await.expect("start bob");
    register_user(&bob, "bob", "Bob", PASSWORD_BOB).await;

    // E2E-чат и первое сообщение от alice1.
    let chat = alice1
        .create_direct_chat("bob", ChatMode::Private)
        .await
        .expect("create e2e chat");
    assert_eq!(chat.e2e_mode, proto::E2eMode::OlmV1);

    let first = "версия один 13579";
    let local_id = alice1
        .send_text(&chat.chat_id, first, None)
        .await
        .expect("send first message");
    wait_sent(&alice1_storage, &local_id).await;
    let sent = alice1
        .list_messages(&chat.chat_id, None, 10)
        .await
        .expect("list own messages")
        .into_iter()
        .find(|message| message.local_id == local_id)
        .expect("own message");
    let server_id = sent.server_id.expect("server id");
    assert_eq!(sent.body.as_deref(), Some(first));

    bob.sync_now().await.expect("sync bob");
    let at_bob = wait_for_body(&bob, &chat.chat_id, first).await;
    assert_eq!(at_bob.server_id, Some(server_id));

    // Bob подтверждает прочтение: автор видит receipt через журнал.
    bob.mark_read(&chat.chat_id, server_id)
        .await
        .expect("mark read");
    alice1.sync_now().await.expect("sync alice1");
    let receipt = wait_for_receipt(&alice1, &chat.chat_id, server_id).await;
    assert!(receipt.read_up_to >= server_id);

    // Второе устройство автора догружает исходное сообщение.
    alice2.sync_now().await.expect("sync alice2");
    let own_copy = wait_for_body(&alice2, &chat.chat_id, first).await;
    assert_eq!(own_copy.server_id, Some(server_id));

    // Правку делает второе устройство автора: новый текст шифруется заново
    // для alice1 и bob.
    let second = "версия два 24680";
    let edited = alice2
        .edit_text(&chat.chat_id, server_id, second)
        .await
        .expect("edit from second device");
    assert_eq!(edited.body.as_deref(), Some(second));
    assert!(edited.edited_at_ms.is_some());

    // Сервер не видел открытый текст правки; у сообщения ровно два новых
    // шифротекста (alice1 и bob).
    assert_server_has_no_plaintext(harness.db_path(), second);
    assert_eq!(message_recipient_count(harness.db_path(), server_id), 2);
    assert_eq!(server_message_plaintext(harness.db_path(), server_id), None);

    // Bob расшифровывает правку: сессия с alice2 создаётся по prekey.
    bob.sync_now().await.expect("sync bob");
    let edited_at_bob = wait_for_edited(&bob, &chat.chat_id, server_id).await;
    assert_eq!(edited_at_bob.body.as_deref(), Some(second));

    // Первое устройство автора тоже видит правку.
    alice1.sync_now().await.expect("sync alice1");
    let edited_at_alice1 = wait_for_edited(&alice1, &chat.chat_id, server_id).await;
    assert_eq!(edited_at_alice1.body.as_deref(), Some(second));

    // Удаление для всех с первого устройства.
    let deleted = alice1
        .delete_message(&chat.chat_id, server_id)
        .await
        .expect("delete message");
    assert!(deleted.deleted_at_ms.is_some());

    // На сервере: ciphertext-ы стёрты, plaintext пуст, открытого текста нет
    // ни в одной таблице.
    assert_eq!(message_recipient_count(harness.db_path(), server_id), 0);
    assert_eq!(server_message_plaintext(harness.db_path(), server_id), None);
    assert_server_has_no_plaintext(harness.db_path(), first);
    assert_server_has_no_plaintext(harness.db_path(), second);

    // Bob получает tombstone без текста.
    bob.sync_now().await.expect("sync bob");
    let deleted_at_bob = wait_for_deleted(&bob, &chat.chat_id, server_id).await;
    assert_eq!(deleted_at_bob.body, None);

    // Второе устройство автора — тоже; локальный текст стёрт.
    alice2.sync_now().await.expect("sync alice2");
    wait_for_deleted(&alice2, &chat.chat_id, server_id).await;

    bob.shutdown().await.expect("shutdown bob");
    alice2.shutdown().await.expect("shutdown alice2");
    alice1.shutdown().await.expect("shutdown alice1");
    harness.shutdown().await;
}

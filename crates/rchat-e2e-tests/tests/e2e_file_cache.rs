//! Локальная дедупликация загрузок и кэш скачанных файлов.
//!
//! - Тот же файл, отправленный дважды, заливается на сервер один раз: хэш
//!   содержимого ищется в локальном кэше загрузок (ключ остаётся на
//!   устройстве, сервер его не знает).
//! - Скачанный файл сохраняется локально, поэтому повторное чтение не
//!   обращается к сети.

mod common;

use common::*;
use rchat_core::payload::{Attachment, AttachmentKind};
use rchat_core::traits::Storage;

fn file_attachment(name: &str, data: &[u8]) -> Attachment {
    Attachment {
        kind: AttachmentKind::File,
        name: name.to_string(),
        mime: "application/octet-stream".to_string(),
        size: data.len() as u64,
        blob_ref: None,
        key: None,
        nonce: None,
        thumb: None,
        waveform: Vec::new(),
        data: data.to_vec(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_same_file_uploaded_once() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let alice_dir = root.path().join("alice");
    let bob_dir = root.path().join("bob");

    let (alice, alice_storage) = build_client(&alice_dir, harness.port(), "alice-dev");
    alice.start().await.expect("start alice");
    register_user(&alice, "alice", "Alice", PASSWORD_ALICE).await;

    let (bob, bob_storage) = build_client(&bob_dir, harness.port(), "bob-dev");
    bob.start().await.expect("start bob");
    register_user(&bob, "bob", "Bob", PASSWORD_BOB).await;

    let chat = alice
        .create_direct_chat("bob", true)
        .await
        .expect("create e2e chat");

    let data = (0u8..=255).cycle().take(64 * 1024).collect::<Vec<u8>>();

    // Первая отправка — файл заливается на сервер.
    let first = alice
        .send_attachment(&chat.chat_id, "раз", file_attachment("a.bin", &data), None)
        .await
        .expect("send first");
    wait_sent(&alice_storage, &first).await;

    // Вторая отправка того же файла — блоб переиспользуется, новой заливки нет.
    let second = alice
        .send_attachment(&chat.chat_id, "два", file_attachment("a.bin", &data), None)
        .await
        .expect("send second");
    wait_sent(&alice_storage, &second).await;

    let first_ref = alice_storage
        .get_message(&first)
        .await
        .expect("get first")
        .expect("first message")
        .attachment
        .expect("attachment")
        .blob_ref
        .expect("blob ref");
    let second_ref = alice_storage
        .get_message(&second)
        .await
        .expect("get second")
        .expect("second message")
        .attachment
        .expect("attachment")
        .blob_ref
        .expect("blob ref");
    assert_eq!(
        first_ref, second_ref,
        "тот же файл должен переиспользовать уже залитый блоб"
    );

    // Получатель видит оба сообщения и может скачать файл.
    let received = wait_for_message(&bob_storage, &chat.chat_id, 2).await;
    let content = bob
        .attachment_content(received.server_id.expect("server id"))
        .await
        .expect("content")
        .expect("attachment");
    assert_eq!(content.data, data, "файл должен совпасть побитово");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_downloaded_file_is_cached_locally() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let alice_dir = root.path().join("alice");
    let bob_dir = root.path().join("bob");

    let (alice, alice_storage) = build_client(&alice_dir, harness.port(), "alice-dev");
    alice.start().await.expect("start alice");
    register_user(&alice, "alice", "Alice", PASSWORD_ALICE).await;

    let (bob, bob_storage) = build_client(&bob_dir, harness.port(), "bob-dev");
    bob.start().await.expect("start bob");
    register_user(&bob, "bob", "Bob", PASSWORD_BOB).await;

    let chat = alice
        .create_direct_chat("bob", true)
        .await
        .expect("create e2e chat");

    let data = vec![42u8; 4096];
    let local_id = alice
        .send_attachment(&chat.chat_id, "файл", file_attachment("b.bin", &data), None)
        .await
        .expect("send");
    wait_sent(&alice_storage, &local_id).await;

    let received = wait_for_message(&bob_storage, &chat.chat_id, 1).await;
    let server_id = received.server_id.expect("server id");

    // Первое чтение скачивает файл и кладёт его в локальный кэш.
    let first = bob
        .attachment_content(server_id)
        .await
        .expect("content")
        .expect("attachment");
    assert_eq!(first.data, data);

    let cached = bob_storage
        .get_message_by_server(server_id)
        .await
        .expect("get message")
        .expect("message")
        .attachment
        .expect("attachment");
    assert_eq!(
        cached.data, data,
        "скачанный файл должен лежать в локальном кэше"
    );
}

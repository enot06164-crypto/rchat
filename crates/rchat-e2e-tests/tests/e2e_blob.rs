//! Сквозной тест канала файлов (блобов) — по образцу Telegram:
//! файл шифруется клиентом случайным ключом, уходит на сервер отдельным
//! блобом, ключ едет внутри зашифрованного сообщения. Сервер не должен видеть
//! ни текст, ни содержимое файла; получатель скачивает блоб и расшифровывает
//! его ключом из сообщения.

mod common;

use common::*;
use rchat_core::payload::{Attachment, AttachmentKind};
use rchat_core::traits::Storage;
use rchat_core::types::ChatMode;
use rchat_proto::proto;

/// Файл заметно больше прежнего лимита кадра сообщения (128 КиБ).
fn payload_bytes(size: usize) -> Vec<u8> {
    (0u8..=255).cycle().take(size).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_blob_channel_delivers_file_encrypted() {
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
        .create_direct_chat("bob", ChatMode::Private)
        .await
        .expect("create e2e chat");
    assert_eq!(chat.e2e_mode, proto::E2eMode::OlmV1);

    // Файл 512 КиБ — он не влезает в кадр сообщения, только в блоб.
    let file = payload_bytes(512 * 1024);
    let secret_text = "вот большой файл";
    let local_id = alice
        .send_attachment(
            &chat.chat_id,
            secret_text,
            Attachment {
                kind: AttachmentKind::File,
                name: "big.bin".into(),
                mime: "application/octet-stream".into(),
                size: file.len() as u64,
                blob_ref: None,
                key: None,
                nonce: None,
                thumb: None,
                waveform: Vec::new(),
                data: file.clone(),
            },
            None,
        )
        .await
        .expect("send attachment");
    wait_sent(&alice_storage, &local_id).await;

    // Локально у отправителя файл не хранится дважды: в сообщении только
    // ссылка на блоб и ключ.
    let stored = alice_storage
        .get_message(&local_id)
        .await
        .expect("get message")
        .expect("message present");
    let attachment = stored.attachment.expect("attachment");
    assert_eq!(attachment.kind, AttachmentKind::File);
    assert_eq!(attachment.size, file.len() as u64);
    assert!(
        attachment.blob_ref.is_some(),
        "в сообщении должна быть ссылка на блоб"
    );
    assert!(
        attachment.key.is_some(),
        "в сообщении должен быть ключ файла"
    );
    assert!(
        attachment.data.is_empty(),
        "файл не должен дублироваться в сообщении"
    );

    // Сервер не видит ни текст, ни содержимое файла.
    let (messages, recipients) = assert_server_has_no_plaintext(harness.db_path(), secret_text);
    assert_eq!(messages, 1, "на сервере должно быть одно сообщение");
    assert_eq!(recipients, 1, "шифротекст должен быть для устройства Bob");
    assert_server_has_no_bytes(harness.db_path(), &file);

    // Получатель расшифровывает конверт, скачивает блоб и получает файл.
    let received = wait_for_message(&bob_storage, &chat.chat_id, 1).await;
    assert_eq!(received.body.as_deref(), Some(secret_text));
    let received_attachment = received.attachment.expect("attachment received");
    assert_eq!(received_attachment.kind, AttachmentKind::File);
    assert_eq!(received_attachment.name, "big.bin");
    assert!(received_attachment.blob_ref.is_some());

    let content = bob
        .attachment_content(received.server_id.expect("server id"))
        .await
        .expect("attachment content")
        .expect("attachment");
    assert_eq!(content.data, file, "файл должен совпасть побитово");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_blob_channel_rejects_oversized_file() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let alice_dir = root.path().join("alice");
    let bob_dir = root.path().join("bob");

    let (alice, _alice_storage) = build_client(&alice_dir, harness.port(), "alice-dev");
    alice.start().await.expect("start alice");
    register_user(&alice, "alice", "Alice", PASSWORD_ALICE).await;

    let (bob, _bob_storage) = build_client(&bob_dir, harness.port(), "bob-dev");
    bob.start().await.expect("start bob");
    register_user(&bob, "bob", "Bob", PASSWORD_BOB).await;

    let chat = alice
        .create_direct_chat("bob", ChatMode::Private)
        .await
        .expect("create e2e chat");

    // Файл больше лимита ядра — отклоняется локально, до загрузки.
    let huge = Attachment {
        kind: AttachmentKind::File,
        name: "huge.bin".into(),
        mime: "application/octet-stream".into(),
        size: rchat_core::MAX_ATTACHMENT_BYTES as u64 + 1,
        blob_ref: None,
        key: None,
        nonce: None,
        thumb: None,
        waveform: Vec::new(),
        data: Vec::new(),
    };
    let error = alice
        .send_attachment(&chat.chat_id, "x", huge, None)
        .await
        .expect_err("oversized attachment must be rejected");
    assert!(
        error.to_string().contains("too large"),
        "неожиданная ошибка: {error}"
    );
}

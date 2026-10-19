//! Сквозной тест вложений: файл/фото/видео/голос едут внутри
//! зашифрованного payload, поэтому сервер не видит ни текст, ни байты;
//! получатель расшифровывает конверт и получает вложение целиком.

mod common;

use common::*;
use rchat_core::payload::{Attachment, AttachmentKind};
use rchat_core::traits::Storage;
use rchat_proto::proto;

fn attachment(kind: AttachmentKind, name: &str, mime: &str, data: &[u8]) -> Attachment {
    Attachment {
        kind,
        name: name.to_string(),
        mime: mime.to_string(),
        size: data.len() as u64,
        data: data.to_vec(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_attachment_is_encrypted_end_to_end() {
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
    assert_eq!(chat.e2e_mode, proto::E2eMode::OlmV1);

    // Подпись + фото.
    let secret_text = "смотри какая картинка";
    let photo_bytes: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
    let local_id = alice
        .send_attachment(
            &chat.chat_id,
            secret_text,
            attachment(AttachmentKind::Photo, "pic.png", "image/png", &photo_bytes),
            None,
        )
        .await
        .expect("send attachment");

    wait_sent(&alice_storage, &local_id).await;

    // Локально вложение сохранилось целиком.
    let stored = alice_storage
        .get_message(&local_id)
        .await
        .expect("get message")
        .expect("message present");
    assert_eq!(stored.content_kind, "photo");
    let stored_attachment = stored.attachment.expect("attachment stored");
    assert_eq!(stored_attachment.kind, AttachmentKind::Photo);
    assert_eq!(stored_attachment.name, "pic.png");
    assert_eq!(stored_attachment.data, photo_bytes);

    // Сервер не видит ни текст, ни содержимое файла.
    let (messages, recipients) = assert_server_has_no_plaintext(harness.db_path(), secret_text);
    assert_eq!(messages, 1, "на сервере должно быть одно сообщение");
    assert_eq!(recipients, 1, "шифротекст должен быть для устройства Bob");
    assert_server_has_no_bytes(harness.db_path(), &photo_bytes);

    // Получатель расшифровывает конверт и получает вложение.
    let received = wait_for_message(&bob_storage, &chat.chat_id, 1).await;
    assert_eq!(received.body.as_deref(), Some(secret_text));
    let received_attachment = received.attachment.expect("attachment received");
    assert_eq!(received_attachment.kind, AttachmentKind::Photo);
    assert_eq!(received_attachment.name, "pic.png");
    assert_eq!(received_attachment.mime, "image/png");
    assert_eq!(received_attachment.size, photo_bytes.len() as u64);
    assert_eq!(received_attachment.data, photo_bytes);

    // Голосовое без подписи: content_kind = voice, текст пустой.
    let voice_bytes = vec![7u8; 512];
    let voice_local_id = alice
        .send_attachment(
            &chat.chat_id,
            "",
            attachment(AttachmentKind::Voice, "voice", "audio/ogg", &voice_bytes),
            None,
        )
        .await
        .expect("send voice");
    wait_sent(&alice_storage, &voice_local_id).await;

    let received_voice = wait_for_message(&bob_storage, &chat.chat_id, 2).await;
    assert_eq!(received_voice.content_kind, "voice");
    assert_eq!(received_voice.body.as_deref(), Some(""));
    let voice = received_voice.attachment.expect("voice attachment");
    assert_eq!(voice.kind, AttachmentKind::Voice);
    assert_eq!(voice.data, voice_bytes);
    assert_server_has_no_bytes(harness.db_path(), &voice_bytes);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_attachment_rejected_when_too_large() {
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
        .create_direct_chat("bob", true)
        .await
        .expect("create e2e chat");

    // Слишком большое вложение отклоняется локально, в outbox не попадает.
    let huge = attachment(
        AttachmentKind::File,
        "huge.bin",
        "application/octet-stream",
        &vec![0u8; rchat_core::MAX_ATTACHMENT_BYTES + 1],
    );
    let error = alice
        .send_attachment(&chat.chat_id, "x", huge, None)
        .await
        .expect_err("oversized attachment must be rejected");
    assert!(
        error.to_string().contains("too large"),
        "неожиданная ошибка: {error}"
    );
}

//! Альбомы (медиагруппы): несколько вложений уходят одним набором с общим
//! `grouped_id` — получатель видит их как один альбом.

mod common;

use common::*;
use rchat_core::payload::{Attachment, AttachmentKind};
use rchat_core::traits::Storage;
use rchat_core::types::ChatMode;

fn photo(name: &str, byte: u8) -> Attachment {
    Attachment {
        kind: AttachmentKind::Photo,
        name: name.to_string(),
        mime: "image/png".to_string(),
        size: 4,
        blob_ref: None,
        key: None,
        nonce: None,
        thumb: None,
        waveform: Vec::new(),
        data: vec![byte; 4],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_album_groups_attachments() {
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

    let local_ids = alice
        .send_album(
            &chat.chat_id,
            "две картинки",
            vec![photo("a.png", 1), photo("b.png", 2)],
            None,
        )
        .await
        .expect("send album");
    assert_eq!(local_ids.len(), 2, "альбом из двух вложений");

    // У отправителя оба сообщения делят grouped_id.
    let mut grouped: Vec<String> = Vec::new();
    for local_id in &local_ids {
        let message = alice_storage
            .get_message(local_id)
            .await
            .expect("get message")
            .expect("message");
        grouped.push(message.grouped_id.expect("grouped id"));
    }
    assert_eq!(
        grouped[0], grouped[1],
        "сообщения альбома должны иметь общий grouped_id"
    );

    // Получатель видит два сообщения с тем же grouped_id.
    let first = wait_for_message(&bob_storage, &chat.chat_id, 2).await;
    let second = wait_for_message(&bob_storage, &chat.chat_id, 1).await;
    assert_eq!(
        first.grouped_id, second.grouped_id,
        "у получателя сообщения альбома должны иметь общий grouped_id"
    );
    assert!(first.grouped_id.is_some());
    // Подпись альбома — на последнем элементе.
    let captioned = if second.body.is_some() {
        &second
    } else {
        &first
    };
    assert_eq!(captioned.body.as_deref(), Some("две картинки"));
}

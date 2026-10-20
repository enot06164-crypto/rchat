//! Облачный режим (как облачные чаты Telegram): сервер знает ключ чата,
//! поэтому умеет искать по истории, а новое устройство аккаунта читает всю
//! историю сразу. Приватный режим (E2E) при этом сохраняет гарантию:
//! сервер не видит содержимого.
//!
//! Проверяемые инварианты:
//! - в облачном чате получатель читает сообщение, отправленное офлайн;
//! - сервер находит сообщение по поиску (ключ чата у него есть);
//! - в приватном чате тот же поиск ничего не находит;
//! - открытый текст в облачном чате не лежит в БД сервера plaintext-полем.

mod common;

use common::*;
use rchat_core::payload::{Attachment, AttachmentKind, BlobRef};
use rchat_core::traits::Storage;
use rchat_core::types::ChatMode;
use rchat_proto::proto;

/// Облачный чат: сообщение доставляется офлайн, сервер ищет по нему.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_chat_delivers_and_is_searchable() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let alice_dir = root.path().join("alice");
    let bob_dir = root.path().join("bob");

    let (alice, alice_storage) = build_client(&alice_dir, harness.port(), "alice-dev");
    alice.start().await.expect("start alice");
    register_user(&alice, "alice", "Alice", PASSWORD_ALICE).await;

    // Bob регистрируется и уходит офлайн.
    {
        let (bob, _bob_storage) = build_client(&bob_dir, harness.port(), "bob-dev");
        bob.start().await.expect("start bob");
        register_user(&bob, "bob", "Bob", PASSWORD_BOB).await;
        bob.shutdown().await.expect("shutdown bob");
    }

    // Облачный чат: по умолчанию именно он.
    assert_eq!(alice.default_chat_mode(), ChatMode::Cloud);
    let chat = alice
        .create_direct_chat("bob", ChatMode::Cloud)
        .await
        .expect("create cloud chat");
    assert_eq!(chat.e2e_mode, proto::E2eMode::Cloud);
    assert_eq!(chat.cloud_key.len(), 32, "у чата есть ключ");

    let secret = "облачное секретное слово 777";
    let local_id = alice
        .send_text(&chat.chat_id, secret, None)
        .await
        .expect("send cloud message");
    wait_sent(&alice_storage, &local_id).await;

    // В БД сервера открытого текста нет: содержимое лежит под ключом чата.
    let plaintexts = server_plaintexts(harness.db_path());
    assert!(
        plaintexts.iter().all(|text| !text.contains(secret)),
        "сервер не должен хранить открытый текст облачного сообщения"
    );

    // Bob читает офлайн-сообщение.
    let (bob, _bob_storage) = build_client(&bob_dir, harness.port(), "bob-dev");
    bob.start().await.expect("start bob again");
    bob.sync_now().await.expect("sync bob");
    let received = wait_for_body(&bob, &chat.chat_id, secret).await;
    assert_eq!(received.content_kind, "text");

    // Сервер ищет по истории: ключ чата у него есть. Ищет автор (для него
    // собеседник в названии чата).
    let hits = alice
        .search_messages("облачное секретное", 10)
        .await
        .expect("search messages");
    assert_eq!(hits.len(), 1, "сервер должен найти облачное сообщение");
    assert_eq!(hits[0].chat_id, chat.chat_id);
    // В выдаче — текст сообщения, а не служебный конверт.
    assert_eq!(hits[0].body, secret);
    assert_eq!(hits[0].chat_title, "Bob");

    // Поиск по несуществующему слову ничего не находит.
    let empty = alice
        .search_messages("такого-слова-нет", 10)
        .await
        .expect("search messages");
    assert!(
        empty.is_empty(),
        "поиск не должен давать ложных срабатываний"
    );
}

/// Приватный чат (E2E) остаётся непрозрачным для сервера: содержимое сервер
/// не видит и по поиску не находит.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn private_chat_is_not_searchable_by_server() {
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
        .expect("create private chat");
    assert_eq!(chat.e2e_mode, proto::E2eMode::OlmV1);
    assert!(chat.cloud_key.is_empty(), "у приватного чата нет ключа");

    let secret = "приватное секретное слово 888";
    let local_id = alice
        .send_text(&chat.chat_id, secret, None)
        .await
        .expect("send private message");
    wait_sent(&alice_storage, &local_id).await;

    // Bob читает (E2E работает как прежде).
    bob.sync_now().await.expect("sync bob");
    wait_for_body(&bob, &chat.chat_id, secret).await;

    // Сервер содержимое не видит: и в БД его нет, и поиск ничего не находит.
    let plaintexts = server_plaintexts(harness.db_path());
    assert!(
        plaintexts.iter().all(|text| !text.contains(secret)),
        "сервер не должен хранить открытый текст приватного сообщения"
    );
    let hits = bob
        .search_messages("приватное секретное", 10)
        .await
        .expect("search messages");
    assert!(
        hits.is_empty(),
        "сервер не должен находить содержимое приватного чата"
    );
    let _ = bob_storage;
}

/// Облачная группа: общий ключ чата, сообщение читают все участники, поиск
/// работает по всей группе.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_group_chat_is_shared_between_members() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let alice_dir = root.path().join("alice");
    let bob_dir = root.path().join("bob");
    let carol_dir = root.path().join("carol");

    let (alice, alice_storage) = build_client(&alice_dir, harness.port(), "alice-dev");
    alice.start().await.expect("start alice");
    register_user(&alice, "alice", "Alice", PASSWORD_ALICE).await;

    let (bob, _bob_storage) = build_client(&bob_dir, harness.port(), "bob-dev");
    bob.start().await.expect("start bob");
    register_user(&bob, "bob", "Bob", PASSWORD_BOB).await;

    let (carol, _carol_storage) = build_client(&carol_dir, harness.port(), "carol-dev");
    carol.start().await.expect("start carol");
    register_user(&carol, "carol", "Carol", "pw-carol-1").await;

    let chat = alice
        .create_group_chat(
            "Проект",
            &["bob".to_string(), "carol".to_string()],
            ChatMode::Cloud,
        )
        .await
        .expect("create cloud group");
    assert_eq!(chat.e2e_mode, proto::E2eMode::Cloud);
    assert_eq!(chat.cloud_key.len(), 32);

    let secret = "общее облачное сообщение 999";
    let local_id = alice
        .send_text(&chat.chat_id, secret, None)
        .await
        .expect("send group message");
    wait_sent(&alice_storage, &local_id).await;

    // Читают оба участника: ключ чата у них общий.
    bob.sync_now().await.expect("sync bob");
    wait_for_body(&bob, &chat.chat_id, secret).await;
    carol.sync_now().await.expect("sync carol");
    wait_for_body(&carol, &chat.chat_id, secret).await;

    // Поиск по группе.
    let hits = bob
        .search_messages("общее облачное", 10)
        .await
        .expect("search messages");
    assert_eq!(hits.len(), 1, "сообщение группы должно находиться");
    assert_eq!(hits[0].body, secret);
    assert_eq!(hits[0].chat_title, "Проект");
}

/// Вложения в облачном чате: фото, видео с постером и альбом едут так же,
/// как в приватном, — файлы лежат отдельными блобами (сервер видит только
/// шифротекст файла), а метаданные и ключ файла — внутри зашифрованного
/// конверта сообщения.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_attachments_travel_as_encrypted_blobs() {
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
        .create_direct_chat("bob", ChatMode::Cloud)
        .await
        .expect("create cloud chat");

    // Фото с подписью.
    let photo_bytes: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
    let local_id = alice
        .send_attachment(
            &chat.chat_id,
            "фото из облака",
            attachment(AttachmentKind::Photo, "pic.png", "image/png", &photo_bytes),
            None,
        )
        .await
        .expect("send photo");
    wait_sent(&alice_storage, &local_id).await;

    // Видео с постером (постер — отдельный блоб, как thumbnail в Telegram).
    let video_bytes: Vec<u8> = (1u8..=255).cycle().take(2048).collect();
    let poster_bytes: Vec<u8> = (9u8..=255).cycle().take(512).collect();
    let mut video = attachment(AttachmentKind::Video, "clip.mp4", "video/mp4", &video_bytes);
    video.thumb = Some(BlobRef {
        reference: String::new(),
        key: Vec::new(),
        nonce: Vec::new(),
        size: poster_bytes.len() as u64,
        mime: "image/jpeg".to_string(),
        data: poster_bytes.clone(),
    });
    let video_id = alice
        .send_attachment(&chat.chat_id, "", video, None)
        .await
        .expect("send video");
    wait_sent(&alice_storage, &video_id).await;

    // Альбом: несколько вложений с общим grouped_id (подпись — на последнем).
    let album_bytes_a: Vec<u8> = vec![11u8; 700];
    let album_bytes_b: Vec<u8> = vec![22u8; 900];
    let album_ids = alice
        .send_album(
            &chat.chat_id,
            "альбом из облака",
            vec![
                attachment(AttachmentKind::Photo, "a.png", "image/png", &album_bytes_a),
                attachment(AttachmentKind::Photo, "b.png", "image/png", &album_bytes_b),
            ],
            None,
        )
        .await
        .expect("send album");
    assert_eq!(album_ids.len(), 2, "альбом — два сообщения");
    for id in &album_ids {
        wait_sent(&alice_storage, id).await;
    }

    // Ни текст, ни байты файла сервер не хранит открытыми.
    assert_server_has_no_bytes(harness.db_path(), &photo_bytes);
    assert_server_has_no_bytes(harness.db_path(), &video_bytes);
    assert_server_has_no_bytes(harness.db_path(), &poster_bytes);

    // Получатель читает оба вложения целиком.
    wait_for_message(&bob_storage, &chat.chat_id, 2).await;
    let received = bob_storage
        .list_messages(&chat.chat_id, None, 10)
        .await
        .expect("list messages");
    let photos = received
        .iter()
        .find(|message| message.body.as_deref() == Some("фото из облака"))
        .expect("photo received");
    assert_eq!(photos.body.as_deref(), Some("фото из облака"));
    let photo = photos.attachment.clone().expect("photo attachment");
    assert_eq!(photo.kind, AttachmentKind::Photo);
    assert!(photo.blob_ref.is_some(), "должна быть ссылка на блоб");
    let content = bob
        .attachment_content(photos.server_id.expect("server id"))
        .await
        .expect("photo content")
        .expect("photo");
    assert_eq!(content.data, photo_bytes, "фото должно совпасть побитово");

    let videos = received
        .iter()
        .find(|message| message.content_kind == "video")
        .expect("video received");
    let video = videos.attachment.clone().expect("video attachment");
    assert_eq!(video.kind, AttachmentKind::Video);
    let thumb = video.thumb.expect("постер");
    assert!(!thumb.reference.is_empty(), "постер — отдельный блоб");
    let video_content = bob
        .attachment_content(videos.server_id.expect("server id"))
        .await
        .expect("video content")
        .expect("video");
    assert_eq!(
        video_content.data, video_bytes,
        "видео должно совпасть побитово"
    );

    // Альбом: у получателя четыре сообщения, два последних — с общим
    // grouped_id и подписью на последнем элементе.
    wait_for_message(&bob_storage, &chat.chat_id, 4).await;
    let received = bob_storage
        .list_messages(&chat.chat_id, None, 10)
        .await
        .expect("list messages");
    let album: Vec<_> = received
        .iter()
        .filter(|message| {
            message
                .grouped_id
                .as_ref()
                .is_some_and(|value| !value.is_empty())
        })
        .collect();
    assert_eq!(album.len(), 2, "альбом — два сообщения с grouped_id");
    assert_eq!(album[0].grouped_id, album[1].grouped_id);
    assert!(album[0].grouped_id.is_some(), "у альбома общий grouped_id");
    // Подпись альбома — на последнем элементе (порядок в выдаче не важен).
    let captions: Vec<&str> = album
        .iter()
        .map(|message| message.body.as_deref().unwrap_or_default())
        .collect();
    assert_eq!(
        captions
            .iter()
            .filter(|text| **text == "альбом из облака")
            .count(),
        1
    );
    assert_eq!(captions.iter().filter(|text| text.is_empty()).count(), 1);
    let mut contents: Vec<Vec<u8>> = Vec::new();
    for message in &album {
        let content = bob
            .attachment_content(message.server_id.expect("server id"))
            .await
            .expect("album content")
            .expect("album attachment");
        contents.push(content.data);
    }
    // Порядок элементов альбома в выдаче не важен — важно, что файлы целые.
    assert!(
        contents.contains(&album_bytes_a) && contents.contains(&album_bytes_b),
        "оба файла альбома должны дойти целыми"
    );
}

/// Облачный файловый канал: сервер хранит файл у себя (как DC в облачных
/// чатах Telegram) и отдаёт его участникам чата, а одинаковое содержимое
/// хранится один раз — второй заливки не происходит.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_file_channel_stores_file_once_and_serves_it() {
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
        .create_direct_chat("bob", ChatMode::Cloud)
        .await
        .expect("create cloud chat");

    let photo_bytes: Vec<u8> = (0u8..=255).cycle().take(3000).collect();
    let local_id = alice
        .send_attachment(
            &chat.chat_id,
            "файл",
            attachment(AttachmentKind::Photo, "pic.png", "image/png", &photo_bytes),
            None,
        )
        .await
        .expect("send photo");
    wait_sent(&alice_storage, &local_id).await;

    // Получатель скачивает файл и получает те же байты.
    wait_for_message(&bob_storage, &chat.chat_id, 1).await;
    let received = bob_storage
        .list_messages(&chat.chat_id, None, 10)
        .await
        .expect("list messages");
    let message = received.first().expect("message");
    let content = bob
        .attachment_content(message.server_id.expect("server id"))
        .await
        .expect("attachment content")
        .expect("attachment");
    assert_eq!(content.data, photo_bytes, "файл должен совпасть побитово");

    // На диске сервера файл зашифрован: открытых байтов там нет.
    let blob_dir = harness.db_path().parent().expect("db dir").join("blobs");
    let files: Vec<_> = std::fs::read_dir(&blob_dir)
        .expect("read blob dir")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().map(|ext| ext == "blob").unwrap_or(false))
        .collect();
    assert_eq!(files.len(), 1, "файл должен лежать на диске один раз");
    let stored = std::fs::read(&files[0]).expect("read blob");
    assert!(
        !contains_bytes(&stored, &photo_bytes),
        "на диске сервера не должно быть открытых байтов файла"
    );

    // Тот же файл от второго отправителя: содержимое не заливается заново —
    // сервер уже знает такой отпечаток.
    let local_id = bob
        .send_attachment(
            &chat.chat_id,
            "тот же файл",
            attachment(AttachmentKind::Photo, "pic.png", "image/png", &photo_bytes),
            None,
        )
        .await
        .expect("send the same photo");
    wait_sent(&bob_storage, &local_id).await;

    let files_after: Vec<_> = std::fs::read_dir(&blob_dir)
        .expect("read blob dir")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().map(|ext| ext == "blob").unwrap_or(false))
        .collect();
    assert_eq!(
        files_after.len(),
        1,
        "одинаковое содержимое должно храниться один раз"
    );
    // Сообщение второго отправителя ссылается на тот же блоб.
    let blob_refs = blob_refs(harness.db_path());
    assert_eq!(
        blob_refs.len(),
        2,
        "у сообщений должны быть ссылки на блоб (ссылка чата своя)"
    );
}

/// Ссылки на блобы из БД сервера (по порядку сообщений).
fn blob_refs(db_path: &std::path::Path) -> Vec<String> {
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open server db");
    let mut stmt = conn
        .prepare("SELECT token FROM blobs WHERE completed = 1 ORDER BY created_at_ms, token")
        .expect("prepare blobs");
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query blobs");
    rows.map(|row| row.expect("row")).collect()
}

/// Есть ли в байтах искомый фрагмент.
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Режим принадлежит чату: если у собеседников разные режимы по
/// умолчанию, второй всё равно открывает существующий чат (без конфликта).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_mode_belongs_to_chat_not_to_caller() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let alice_dir = root.path().join("alice");
    let bob_dir = root.path().join("bob");

    let (alice, _alice_storage) = build_client(&alice_dir, harness.port(), "alice-dev");
    alice.start().await.expect("start alice");
    register_user(&alice, "alice", "Alice", PASSWORD_ALICE).await;
    alice
        .set_default_chat_mode(ChatMode::Private)
        .await
        .expect("set mode");

    let (bob, _bob_storage) = build_client(&bob_dir, harness.port(), "bob-dev");
    bob.start().await.expect("start bob");
    register_user(&bob, "bob", "Bob", PASSWORD_BOB).await;
    // У Боба по умолчанию облачный режим.
    assert_eq!(bob.default_chat_mode(), ChatMode::Cloud);

    // Алиса создаёт приватный чат.
    let chat = alice
        .create_direct_chat("bob", ChatMode::Private)
        .await
        .expect("create chat");
    assert_eq!(chat.e2e_mode, proto::E2eMode::OlmV1);

    // Боб с облачным режимом по умолчанию открывает тот же чат — режим
    // остаётся приватным (принадлежит чату), конфликта нет.
    let same = bob
        .create_direct_chat("alice", ChatMode::Cloud)
        .await
        .expect("open the same chat");
    assert_eq!(same.chat_id, chat.chat_id);
    assert_eq!(same.e2e_mode, proto::E2eMode::OlmV1);
}

/// Поиск пользователей по username — для выбора собеседника.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn users_are_searchable_by_username_prefix() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let alice_dir = root.path().join("alice");
    let bob_dir = root.path().join("bob");

    let (alice, _alice_storage) = build_client(&alice_dir, harness.port(), "alice-dev");
    alice.start().await.expect("start alice");
    register_user(&alice, "alice", "Alice", PASSWORD_ALICE).await;

    let (bob, _bob_storage) = build_client(&bob_dir, harness.port(), "bob-dev");
    bob.start().await.expect("start bob");
    register_user(&bob, "bobby", "Bobby", PASSWORD_BOB).await;

    let found = alice.search_users("bob", 10).await.expect("search users");
    assert_eq!(found.len(), 1, "по префиксу найден один пользователь");
    assert_eq!(found[0].username, "bobby");
    assert_eq!(found[0].display_name, "Bobby");

    // Себя в выдаче нет.
    let all = alice.search_users("a", 20).await.expect("search users");
    assert!(
        all.iter().all(|user| user.username != "alice"),
        "самого себя в поиске быть не должно"
    );
    // Пустой запрос — пустая выдача.
    let empty = alice.search_users("", 10).await.expect("search users");
    assert!(empty.is_empty());
}

/// Режим по умолчанию меняется в настройках клиента и применяется к новым
/// чатам; уже созданные чаты сохраняют свой режим.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn default_mode_is_configurable() {
    let harness = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let alice_dir = root.path().join("alice");

    let (alice, _storage) = build_client(&alice_dir, harness.port(), "alice-dev");
    alice.start().await.expect("start alice");
    register_user(&alice, "alice", "Alice", PASSWORD_ALICE).await;

    assert_eq!(alice.default_chat_mode(), ChatMode::Cloud);
    alice
        .set_default_chat_mode(ChatMode::Private)
        .await
        .expect("set default mode");
    assert_eq!(alice.default_chat_mode(), ChatMode::Private);
    alice
        .set_default_chat_mode(ChatMode::Cloud)
        .await
        .expect("set default mode back");
    assert_eq!(alice.default_chat_mode(), ChatMode::Cloud);
}

/// Вложение для тестов: файл уедет отдельным блобом, ключ — внутрь
/// зашифрованного конверта сообщения.
fn attachment(kind: AttachmentKind, name: &str, mime: &str, data: &[u8]) -> Attachment {
    Attachment {
        kind,
        name: name.to_string(),
        mime: mime.to_string(),
        size: data.len() as u64,
        blob_ref: None,
        key: None,
        nonce: None,
        thumb: None,
        waveform: Vec::new(),
        data: data.to_vec(),
    }
}

/// Открытые тексты всех сообщений сервера (для проверок приватности).
fn server_plaintexts(db_path: &std::path::Path) -> Vec<String> {
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open server db");
    let mut stmt = conn
        .prepare("SELECT plaintext_dev FROM messages")
        .expect("prepare messages");
    let rows = stmt
        .query_map([], |row| row.get::<_, Option<String>>(0))
        .expect("query messages");
    rows.map(|row| row.expect("row").unwrap_or_default())
        .collect()
}

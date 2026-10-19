//! Smoke-тесты CLI: реальный сервер (in-process, без TLS) и настоящий
//! бинарник `rchat-cli` как отдельные процессы.
//!
//! Проверяются основные пользовательские сценарии: регистрация двух
//! пользователей, личный чат, отправка, синхронизация и история, а также
//! офлайн-очередь с автоматической выгрузкой после переподключения.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use rchat_testkit::TestServer;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};

/// Поднимает сервер на указанном каталоге БД (для рестартов с той же БД).
async fn start_server_in(db_dir: &Path) -> TestServer {
    TestServer::start_with_db(db_dir.join("server.sqlite3"), |_| {}).await
}

/// Сервер со своей временной БД (тестовые параметры Argon2 — из testkit).
async fn start_server() -> TestServer {
    TestServer::start().await
}

/// Запускает настоящий бинарник CLI и возвращает (код, stdout, stderr).
async fn cli(
    data_dir: &Path,
    port: u16,
    args: &[&str],
    stdin: Option<&str>,
) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_rchat-cli");
    let mut command = Command::new(bin);
    command
        .arg("--server")
        .arg(format!("ws://127.0.0.1:{port}"))
        .arg("--data-dir")
        .arg(data_dir)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn cli");
    if let Some(input) = stdin {
        let mut handle = child.stdin.take().expect("stdin");
        handle
            .write_all(input.as_bytes())
            .await
            .expect("write stdin");
        handle.shutdown().await.expect("close stdin");
    }
    let output = child.wait_with_output().await.expect("wait cli");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn assert_ok(code: i32, stdout: &str, stderr: &str, context: &str) {
    assert_eq!(
        code, 0,
        "{context}: код {code}\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// Запускает `watch` с захватом stdout для чтения событий по строкам.
async fn spawn_watch(data_dir: &Path, port: u16) -> Child {
    Command::new(env!("CARGO_BIN_EXE_rchat-cli"))
        .arg("--server")
        .arg(format!("ws://127.0.0.1:{port}"))
        .arg("--data-dir")
        .arg(data_dir)
        .arg("watch")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn watch")
}

/// Запускает `call invite --wait` с захватом stdout.
async fn spawn_call_invite(data_dir: &Path, port: u16, chat_id: &str) -> Child {
    Command::new(env!("CARGO_BIN_EXE_rchat-cli"))
        .arg("--server")
        .arg(format!("ws://127.0.0.1:{port}"))
        .arg("--data-dir")
        .arg(data_dir)
        .args(["call", "invite", chat_id, "--wait"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn call invite")
}

/// Читает строки до первой, содержащей `needle` (с общим таймаутом).
async fn read_until_line(
    reader: &mut BufReader<ChildStdout>,
    needle: &str,
    context: &str,
) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut seen = String::new();
    loop {
        let mut line = String::new();
        match tokio::time::timeout_at(deadline, reader.read_line(&mut line)).await {
            Ok(Ok(0)) => panic!("{context}: EOF, ждали {needle:?}; получено:\n{seen}"),
            Ok(Ok(_)) => {
                seen.push_str(&line);
                if line.contains(needle) {
                    return line;
                }
            }
            Ok(Err(error)) => panic!("{context}: ошибка чтения {error}; получено:\n{seen}"),
            Err(_) => panic!("{context}: таймаут, ждали {needle:?}; получено:\n{seen}"),
        }
    }
}

/// Достаёт 32-символьный hex-идентификатор звонка из строки вывода.
fn call_id_from_line(line: &str) -> String {
    line.split_whitespace()
        .find(|token| token.len() == 32 && token.chars().all(|ch| ch.is_ascii_hexdigit()))
        .unwrap_or_else(|| panic!("нет call_id в строке: {line:?}"))
        .to_string()
}

/// Регистрирует пользователя и выполняет вход (пароль — через stdin).
async fn register(data_dir: &Path, port: u16, username: &str, password: &str) -> String {
    let (code, stdout, stderr) = cli(
        data_dir,
        port,
        &["register", username, "--password-stdin"],
        Some(&format!("{password}\n")),
    )
    .await;
    assert_ok(code, &stdout, &stderr, &format!("register {username}"));
    stdout
}

/// Создаёт личный чат alice → bob и возвращает его идентификатор.
async fn create_chat(alice_dir: &Path, port: u16, peer: &str) -> String {
    let (code, stdout, stderr) = cli(alice_dir, port, &["chat-create", peer], None).await;
    assert_ok(code, &stdout, &stderr, "chat-create");
    stdout
        .lines()
        .next()
        .expect("первая строка вывода")
        .rsplit(": ")
        .next()
        .expect("chat_id")
        .trim()
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn register_chat_send_sync_history() {
    let server = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let alice_dir = root.path().join("alice");
    let bob_dir = root.path().join("bob");

    let stdout = register(&alice_dir, server.port(), "alice", "secret-alice").await;
    assert!(stdout.contains("Зарегистрирован"), "stdout: {stdout}");
    register(&bob_dir, server.port(), "bob", "secret-bob").await;

    let chat_id = create_chat(&alice_dir, server.port(), "bob").await;
    assert!(!chat_id.is_empty());

    // Отправка с подтверждением сервера.
    let (code, stdout, stderr) = cli(
        &alice_dir,
        server.port(),
        &["send", &chat_id, "Привет, bob!"],
        None,
    )
    .await;
    assert_ok(code, &stdout, &stderr, "send alice");
    assert!(stdout.contains("Отправлено."), "stdout: {stdout}");

    // Второй клиент получает сообщение синхронизацией.
    let (code, stdout, stderr) = cli(&bob_dir, server.port(), &["sync"], None).await;
    assert_ok(code, &stdout, &stderr, "sync bob");
    assert!(stdout.contains("применено 2"), "stdout: {stdout}");

    let (code, stdout, stderr) = cli(&bob_dir, server.port(), &["history", &chat_id], None).await;
    assert_ok(code, &stdout, &stderr, "history bob");
    assert!(stdout.contains("Привет, bob!"), "stdout: {stdout}");

    // Ответ и повторная синхронизация первого клиента.
    let (code, stdout, stderr) = cli(
        &bob_dir,
        server.port(),
        &["send", &chat_id, "И тебе привет"],
        None,
    )
    .await;
    assert_ok(code, &stdout, &stderr, "send bob");

    let (code, stdout, stderr) = cli(&alice_dir, server.port(), &["sync"], None).await;
    assert_ok(code, &stdout, &stderr, "sync alice");

    let (code, stdout, stderr) = cli(&alice_dir, server.port(), &["history", &chat_id], None).await;
    assert_ok(code, &stdout, &stderr, "history alice");
    assert!(stdout.contains("И тебе привет"), "stdout: {stdout}");

    // Список чатов берётся из локального кеша.
    let (code, stdout, stderr) = cli(&alice_dir, server.port(), &["chats"], None).await;
    assert_ok(code, &stdout, &stderr, "chats alice");
    assert!(stdout.contains(&chat_id), "stdout: {stdout}");
}

/// Создаёт E2E-чат alice → bob и возвращает его идентификатор.
async fn create_e2e_chat(alice_dir: &Path, port: u16, peer: &str) -> String {
    let (code, stdout, stderr) = cli(
        alice_dir,
        port,
        &["chat-create", peer, "--mode", "private"],
        None,
    )
    .await;
    assert_ok(code, &stdout, &stderr, "chat-create --mode private");
    assert!(stdout.contains("E2E"), "stdout: {stdout}");
    stdout
        .lines()
        .next()
        .expect("первая строка вывода")
        .rsplit(": ")
        .next()
        .expect("chat_id")
        .trim()
        .to_string()
}

/// Путь серверной БД тестового сервера на временном каталоге.
fn server_db_path(server: &TestServer) -> std::path::PathBuf {
    server.db_path().to_path_buf()
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Проверяет, что в серверной БД нет открытого текста E2E-сообщений,
/// а шифротексты получателей на месте.
fn assert_server_has_no_plaintext(db: &Path, needle: &str, min_recipients: usize) {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open server db");

    let mut stmt = conn
        .prepare("SELECT plaintext_dev FROM messages")
        .expect("prepare messages");
    let rows = stmt
        .query_map([], |row| row.get::<_, Option<String>>(0))
        .expect("query messages");
    for row in rows {
        if let Some(text) = row.expect("row") {
            assert!(
                !text.contains(needle),
                "messages.plaintext_dev содержит открытый текст"
            );
        }
    }

    let mut stmt = conn
        .prepare("SELECT ciphertext FROM message_recipients")
        .expect("prepare recipients");
    let rows = stmt
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .expect("query recipients");
    let mut recipients = 0usize;
    for row in rows {
        let value = row.expect("row");
        recipients += 1;
        assert!(!value.is_empty(), "шифротекст получателя пуст");
        assert!(
            !contains_bytes(&value, needle.as_bytes()),
            "ciphertext содержит открытый текст"
        );
    }
    assert!(
        recipients >= min_recipients,
        "ожидалось минимум {min_recipients} шифротекстов, найдено {recipients}"
    );

    let mut stmt = conn
        .prepare("SELECT payload FROM sync_events")
        .expect("prepare events");
    let rows = stmt
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .expect("query events");
    for row in rows {
        let value = row.expect("row");
        assert!(
            !contains_bytes(&value, needle.as_bytes()),
            "sync_events.payload содержит открытый текст"
        );
    }
}

/// Сквозной E2E-сценарий через настоящий бинарник CLI: E2E-чат, отправка,
/// синхронизация, расшифровка вторым клиентом и проверка, что серверная БД
/// не содержит открытый текст.
#[tokio::test(flavor = "multi_thread")]
async fn e2e_chat_send_sync_history() {
    let server = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let alice_dir = root.path().join("alice");
    let bob_dir = root.path().join("bob");

    let stdout = register(&alice_dir, server.port(), "alice", "secret-alice").await;
    assert!(stdout.contains("Зарегистрирован"), "stdout: {stdout}");
    register(&bob_dir, server.port(), "bob", "secret-bob").await;

    let chat_id = create_e2e_chat(&alice_dir, server.port(), "bob").await;
    assert!(!chat_id.is_empty());

    // Отправка под E2E: на сервер уходит только шифротекст.
    let secret = "очень секретный текст 13579";
    let (code, stdout, stderr) =
        cli(&alice_dir, server.port(), &["send", &chat_id, secret], None).await;
    assert_ok(code, &stdout, &stderr, "send e2e");
    assert!(stdout.contains("Отправлено."), "stdout: {stdout}");

    let db = server_db_path(&server);
    assert_server_has_no_plaintext(&db, secret, 1);

    // Получатель синхронизируется и расшифровывает.
    let (code, stdout, stderr) = cli(&bob_dir, server.port(), &["sync"], None).await;
    assert_ok(code, &stdout, &stderr, "sync bob");
    let (code, stdout, stderr) = cli(&bob_dir, server.port(), &["history", &chat_id], None).await;
    assert_ok(code, &stdout, &stderr, "history bob");
    assert!(stdout.contains(secret), "stdout: {stdout}");

    // Ответ: сессия уже установлена, тоже E2E.
    let reply = "ответ 24680";
    let (code, stdout, stderr) =
        cli(&bob_dir, server.port(), &["send", &chat_id, reply], None).await;
    assert_ok(code, &stdout, &stderr, "send reply");
    assert_server_has_no_plaintext(&db, reply, 2);

    let (code, stdout, stderr) = cli(&alice_dir, server.port(), &["sync"], None).await;
    assert_ok(code, &stdout, &stderr, "sync alice");
    let (code, stdout, stderr) = cli(&alice_dir, server.port(), &["history", &chat_id], None).await;
    assert_ok(code, &stdout, &stderr, "history alice");
    assert!(stdout.contains(secret), "stdout: {stdout}");
    assert!(stdout.contains(reply), "stdout: {stdout}");

    // Отпечатки устройств участников доступны для проверки.
    let (code, stdout, stderr) = cli(
        &alice_dir,
        server.port(),
        &["devices", "--chat", "bob"],
        None,
    )
    .await;
    assert_ok(code, &stdout, &stderr, "devices --chat");
    assert!(stdout.contains("ключ:"), "stdout: {stdout}");
}

/// Этап 7 через настоящий бинарник: правка, удаление, отметка прочтения и
/// receipts. Bob «офлайн» в момент правки и удаления — события догружаются
/// его следующим запуском (каждый запуск CLI — отдельный процесс и sync).
#[tokio::test(flavor = "multi_thread")]
async fn edit_delete_read_and_receipts_flow() {
    let server = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let alice_dir = root.path().join("alice");
    let bob_dir = root.path().join("bob");

    register(&alice_dir, server.port(), "alice", "secret-alice").await;
    register(&bob_dir, server.port(), "bob", "secret-bob").await;
    let chat_id = create_chat(&alice_dir, server.port(), "bob").await;

    // Два сообщения: server_id 1 и 2 (свежая БД сервера).
    for text in ["первое сообщение", "второе сообщение"] {
        let (code, stdout, stderr) =
            cli(&alice_dir, server.port(), &["send", &chat_id, text], None).await;
        assert_ok(code, &stdout, &stderr, "send");
    }

    // Bob синхронизируется и прочитывает оба.
    let (code, stdout, stderr) = cli(&bob_dir, server.port(), &["sync"], None).await;
    assert_ok(code, &stdout, &stderr, "sync bob");
    let (code, stdout, stderr) = cli(&bob_dir, server.port(), &["history", &chat_id], None).await;
    assert_ok(code, &stdout, &stderr, "history bob");
    assert!(stdout.contains("первое сообщение"), "stdout: {stdout}");
    assert!(stdout.contains("второе сообщение"), "stdout: {stdout}");

    let (code, stdout, stderr) = cli(&bob_dir, server.port(), &["read", &chat_id], None).await;
    assert_ok(code, &stdout, &stderr, "read bob");
    assert!(stdout.contains("до #2"), "stdout: {stdout}");

    // Автор видит подтверждение прочтения.
    let (code, stdout, stderr) = cli(&alice_dir, server.port(), &["sync"], None).await;
    assert_ok(code, &stdout, &stderr, "sync alice");
    let (code, stdout, stderr) =
        cli(&alice_dir, server.port(), &["receipts", &chat_id], None).await;
    assert_ok(code, &stdout, &stderr, "receipts alice");
    assert!(stdout.contains("прочитано до #2"), "stdout: {stdout}");

    // Bob «уходит в офлайн». Alice правит первое и удаляет второе.
    let (code, stdout, stderr) = cli(
        &alice_dir,
        server.port(),
        &["edit", &chat_id, "1", "первое+"],
        None,
    )
    .await;
    assert_ok(code, &stdout, &stderr, "edit alice");
    assert!(stdout.contains("изменено"), "stdout: {stdout}");
    let (code, stdout, stderr) =
        cli(&alice_dir, server.port(), &["delete", &chat_id, "2"], None).await;
    assert_ok(code, &stdout, &stderr, "delete alice");
    assert!(stdout.contains("удалено для всех"), "stdout: {stdout}");

    // История автора сразу видит правку и tombstone.
    let (code, stdout, stderr) = cli(&alice_dir, server.port(), &["history", &chat_id], None).await;
    assert_ok(code, &stdout, &stderr, "history alice");
    assert!(stdout.contains("первое+"), "stdout: {stdout}");
    assert!(stdout.contains("(изменено"), "stdout: {stdout}");
    assert!(stdout.contains("(удалено)"), "stdout: {stdout}");
    assert!(!stdout.contains("второе сообщение"), "stdout: {stdout}");

    // Bob догружает события после «отключения»: правка и удаление видны.
    let (code, stdout, stderr) = cli(&bob_dir, server.port(), &["sync"], None).await;
    assert_ok(code, &stdout, &stderr, "sync bob after offline");
    let (code, stdout, stderr) = cli(&bob_dir, server.port(), &["history", &chat_id], None).await;
    assert_ok(code, &stdout, &stderr, "history bob after offline");
    assert!(stdout.contains("первое+"), "stdout: {stdout}");
    assert!(stdout.contains("(изменено"), "stdout: {stdout}");
    assert!(stdout.contains("(удалено)"), "stdout: {stdout}");
    assert!(!stdout.contains("второе сообщение"), "stdout: {stdout}");

    // Чужое сообщение править и удалять нельзя.
    let (code, stdout, stderr) = cli(
        &bob_dir,
        server.port(),
        &["edit", &chat_id, "1", "взлом"],
        None,
    )
    .await;
    assert_ne!(
        code, 0,
        "правка чужого сообщения должна падать\nstdout: {stdout}\nstderr: {stderr}"
    );
    let (code, stdout, stderr) =
        cli(&bob_dir, server.port(), &["delete", &chat_id, "1"], None).await;
    assert_ne!(
        code, 0,
        "удаление чужого сообщения должно падать\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_message_is_flushed_after_reconnect() {
    let root = tempfile::tempdir().expect("tempdir");
    let alice_dir = root.path().join("alice");
    let bob_dir = root.path().join("bob");

    // Первый запуск сервера: регистрация и общий чат.
    let first = start_server_in(root.path()).await;
    register(&alice_dir, first.port(), "alice", "secret-alice").await;
    register(&bob_dir, first.port(), "bob", "secret-bob").await;
    let chat_id = create_chat(&alice_dir, first.port(), "bob").await;
    let first_port = first.port();
    first.shutdown().await;

    // Сервер выключен: сообщение остаётся в очереди.
    let (code, stdout, stderr) = cli(
        &alice_dir,
        first_port,
        &["send", &chat_id, "Сообщение из офлайна", "--wait", "1"],
        None,
    )
    .await;
    assert_ok(code, &stdout, &stderr, "offline send");
    assert!(stdout.contains("остаётся в очереди"), "stdout: {stdout}");

    let (code, stdout, stderr) = cli(&alice_dir, first_port, &["status"], None).await;
    assert_ok(code, &stdout, &stderr, "status offline");
    assert!(stdout.contains("Очередь отправки: 1"), "stdout: {stdout}");

    // Сервер снова поднят на той же БД (другой порт): сессия и чат живы.
    let second = start_server_in(root.path()).await;
    let (code, stdout, stderr) = cli(&alice_dir, second.port(), &["sync"], None).await;
    assert_ok(code, &stdout, &stderr, "sync after reconnect");
    assert!(
        stdout.contains("Очередь отправки пуста."),
        "stdout: {stdout}"
    );

    let (code, stdout, stderr) = cli(&alice_dir, second.port(), &["history", &chat_id], None).await;
    assert_ok(code, &stdout, &stderr, "history after flush");
    assert!(stdout.contains("Сообщение из офлайна"), "stdout: {stdout}");
    assert!(stdout.contains("[отправлено]"), "stdout: {stdout}");
    assert!(!stdout.contains("[в очереди]"), "stdout: {stdout}");

    // Второй клиент видит доставленное сообщение.
    let (code, stdout, stderr) = cli(&bob_dir, second.port(), &["sync"], None).await;
    assert_ok(code, &stdout, &stderr, "sync bob");
    let (code, stdout, stderr) = cli(&bob_dir, second.port(), &["history", &chat_id], None).await;
    assert_ok(code, &stdout, &stderr, "history bob");
    assert!(stdout.contains("Сообщение из офлайна"), "stdout: {stdout}");
}

/// Число получательских копий последнего сообщения в серверной БД и длина
/// общего megolm-шифротекста.
fn last_message_stats(db: &Path) -> (u64, usize, usize) {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open server db");
    let (server_id, ciphertext_len) = conn
        .query_row(
            "SELECT server_id, length(COALESCE(group_ciphertext, x'')) FROM messages
             ORDER BY server_id DESC LIMIT 1",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .expect("last message");
    let recipients = conn
        .query_row(
            "SELECT COUNT(*) FROM message_recipients WHERE server_id = ?1",
            [server_id],
            |row| row.get::<_, i64>(0),
        )
        .expect("recipients");
    (
        server_id as u64,
        recipients as usize,
        ciphertext_len as usize,
    )
}

/// Создаёт группу через CLI и возвращает её `chat_id`.
async fn create_group(alice_dir: &Path, port: u16, name: &str, members: &str) -> String {
    let (code, stdout, stderr) = cli(
        alice_dir,
        port,
        &[
            "group-create",
            name,
            "--members",
            members,
            "--mode",
            "private",
        ],
        None,
    )
    .await;
    assert_ok(code, &stdout, &stderr, "group-create");
    assert!(stdout.contains("E2E"), "stdout: {stdout}");
    stdout
        .lines()
        .next()
        .expect("первая строка вывода")
        .rsplit(": ")
        .next()
        .expect("chat_id")
        .trim()
        .to_string()
}

/// Групповой E2E-чат через настоящий бинарник CLI: создание, обмен
/// сообщениями, ротация ключа при выходе участника, исключение и
/// переименование.
#[tokio::test(flavor = "multi_thread")]
async fn group_chat_lifecycle() {
    let server = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let alice_dir = root.path().join("alice");
    let bob_dir = root.path().join("bob");
    let carol_dir = root.path().join("carol");

    register(&alice_dir, server.port(), "alice", "secret-alice").await;
    register(&bob_dir, server.port(), "bob", "secret-bob").await;
    register(&carol_dir, server.port(), "carol", "secret-carol").await;

    let chat_id = create_group(&alice_dir, server.port(), "Проект", "bob,carol").await;

    // Состав и роли видит владелец.
    let (code, stdout, stderr) = cli(
        &alice_dir,
        server.port(),
        &["group", "info", &chat_id],
        None,
    )
    .await;
    assert_ok(code, &stdout, &stderr, "group info");
    assert!(stdout.contains("Участники (3)"), "stdout: {stdout}");
    assert!(stdout.contains("владелец"), "stdout: {stdout}");

    // Первое сообщение: сервер хранит только шифротекст и ключи участников.
    let secret = "план на неделю 246";
    let (code, stdout, stderr) =
        cli(&alice_dir, server.port(), &["send", &chat_id, secret], None).await;
    assert_ok(code, &stdout, &stderr, "send to group");
    assert!(stdout.contains("Отправлено."), "stdout: {stdout}");
    let db = server_db_path(&server);
    assert_server_has_no_plaintext(&db, secret, 2);
    let (_, recipients, ciphertext_len) = last_message_stats(&db);
    assert_eq!(recipients, 2, "ключ выдан двум участникам");
    assert!(ciphertext_len > 0, "megolm-шифротекст на месте");

    // Получатели расшифровывают.
    for (dir, who) in [(&bob_dir, "bob"), (&carol_dir, "carol")] {
        let (code, stdout, stderr) = cli(dir, server.port(), &["sync"], None).await;
        assert_ok(code, &stdout, &stderr, &format!("sync {who}"));
        let (code, stdout, stderr) = cli(dir, server.port(), &["history", &chat_id], None).await;
        assert_ok(code, &stdout, &stderr, &format!("history {who}"));
        assert!(stdout.contains(secret), "{who} stdout: {stdout}");
    }

    // Carol покидает группу; владелец видит состав без неё.
    let (code, stdout, stderr) = cli(
        &carol_dir,
        server.port(),
        &["group", "leave", &chat_id],
        None,
    )
    .await;
    assert_ok(code, &stdout, &stderr, "group leave");
    assert!(stdout.contains("вышли из группы"), "stdout: {stdout}");

    let (code, stdout, stderr) = cli(&alice_dir, server.port(), &["sync"], None).await;
    assert_ok(code, &stdout, &stderr, "sync alice");
    let (code, stdout, stderr) = cli(
        &alice_dir,
        server.port(),
        &["group", "info", &chat_id],
        None,
    )
    .await;
    assert_ok(code, &stdout, &stderr, "group info after leave");
    assert!(stdout.contains("Участники (2)"), "stdout: {stdout}");
    assert!(!stdout.contains("carol ("), "stdout: {stdout}");

    // Новое сообщение: сессия ротирована, ключ уходит только Bob.
    let after = "после выхода 357";
    let (code, stdout, stderr) =
        cli(&alice_dir, server.port(), &["send", &chat_id, after], None).await;
    assert_ok(code, &stdout, &stderr, "send after leave");
    let (_, recipients, ciphertext_len) = last_message_stats(&db);
    assert_eq!(recipients, 1, "ключ уходит только оставшемуся участнику");
    assert!(ciphertext_len > 0, "megolm-шифротекст на месте");
    assert_server_has_no_plaintext(&db, after, 0);

    // Bob читает новое и старое; Carol — только старое (её больше нет в чате).
    let (code, stdout, stderr) = cli(&bob_dir, server.port(), &["sync"], None).await;
    assert_ok(code, &stdout, &stderr, "sync bob");
    let (code, stdout, stderr) = cli(&bob_dir, server.port(), &["history", &chat_id], None).await;
    assert_ok(code, &stdout, &stderr, "history bob");
    assert!(stdout.contains(secret), "stdout: {stdout}");
    assert!(stdout.contains(after), "stdout: {stdout}");

    let (code, stdout, stderr) = cli(&carol_dir, server.port(), &["sync"], None).await;
    assert_ok(code, &stdout, &stderr, "sync carol");
    let (code, stdout, stderr) = cli(&carol_dir, server.port(), &["history", &chat_id], None).await;
    assert_ok(code, &stdout, &stderr, "history carol");
    assert!(stdout.contains(secret), "stdout: {stdout}");
    assert!(!stdout.contains(after), "carol stdout: {stdout}");

    // Исключённая участница больше не может писать в группу.
    let (code, stdout, stderr) = cli(
        &carol_dir,
        server.port(),
        &["send", &chat_id, "ещё одно", "--wait", "3"],
        None,
    )
    .await;
    assert_ne!(code, 0, "отправка исключённой должна быть отклонена");
    // Отказ может проявиться и при сборке (устройства участников уже не видны
    // вне чата), и при проверке на сервере — в обоих случаях сообщение не уходит.
    assert!(
        !stdout.contains("Отправлено.") && format!("{stdout}{stderr}").contains("ошибка"),
        "stdout: {stdout}\nstderr: {stderr}"
    );

    // Bob не владелец — исключать участников не может.
    let (code, _, _) = cli(
        &bob_dir,
        server.port(),
        &["group", "remove", &chat_id, "alice"],
        None,
    )
    .await;
    assert_ne!(code, 0, "исключать может только владелец");

    // Переименование владельцем.
    let (code, stdout, stderr) = cli(
        &alice_dir,
        server.port(),
        &["group", "rename", &chat_id, "Проект-2"],
        None,
    )
    .await;
    assert_ok(code, &stdout, &stderr, "group rename");
    assert!(stdout.contains("Проект-2"), "stdout: {stdout}");

    let (code, stdout, stderr) = cli(&bob_dir, server.port(), &["sync"], None).await;
    assert_ok(code, &stdout, &stderr, "sync bob");
    let (code, stdout, stderr) = cli(&bob_dir, server.port(), &["chats"], None).await;
    assert_ok(code, &stdout, &stderr, "chats bob");
    assert!(stdout.contains("Проект-2"), "stdout: {stdout}");

    // Исключение участника владельцем.
    let (code, stdout, stderr) = cli(
        &alice_dir,
        server.port(),
        &["group", "remove", &chat_id, "bob"],
        None,
    )
    .await;
    assert_ok(code, &stdout, &stderr, "group remove");
    assert!(stdout.contains("исключён"), "stdout: {stdout}");

    let (code, stdout, stderr) = cli(&bob_dir, server.port(), &["sync"], None).await;
    assert_ok(code, &stdout, &stderr, "sync bob");
    let (code, stdout, stderr) =
        cli(&bob_dir, server.port(), &["group", "info", &chat_id], None).await;
    assert_ok(code, &stdout, &stderr, "group info after kick");
    assert!(stdout.contains("Участники (1)"), "stdout: {stdout}");
}

/// Несколько аккаунтов в одном каталоге данных: `--account` изолирует БД,
/// секреты и E2E-состояние, `accounts` показывает профили, а
/// `account-delete` удаляет только локальные данные выбранного профиля.
#[tokio::test(flavor = "multi_thread")]
async fn accounts_profiles_are_isolated() {
    let server = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let data = root.path().join("data");

    // `alice` — профиль default прямо в каталоге; `bob` — именованный.
    let (code, stdout, stderr) = cli(
        &data,
        server.port(),
        &["register", "alice", "--password-stdin"],
        Some("secret-alice\n"),
    )
    .await;
    assert_ok(code, &stdout, &stderr, "register default alice");
    let (code, stdout, stderr) = cli(
        &data,
        server.port(),
        &["--account", "bob", "register", "bob", "--password-stdin"],
        Some("secret-bob\n"),
    )
    .await;
    assert_ok(code, &stdout, &stderr, "register bob profile");

    assert!(
        data.join("client.sqlite3").is_file(),
        "профиль default лежит прямо в каталоге данных"
    );
    assert!(
        data.join("accounts/bob/client.sqlite3").is_file(),
        "именованный профиль лежит в accounts/<имя>"
    );

    // Список профилей: видны оба, текущий помечен `*`.
    let (code, stdout, stderr) = cli(
        &data,
        server.port(),
        &["--account", "bob", "accounts"],
        None,
    )
    .await;
    assert_ok(code, &stdout, &stderr, "accounts");
    assert!(stdout.contains("* bob —"), "stdout: {stdout}");
    assert!(stdout.contains(" default —"), "stdout: {stdout}");

    // `me` называет активный профиль.
    let (code, stdout, stderr) = cli(&data, server.port(), &["--account", "bob", "me"], None).await;
    assert_ok(code, &stdout, &stderr, "me bob");
    assert!(stdout.contains("Аккаунт: bob"), "stdout: {stdout}");

    // Переписка: default-alice → именованный bob.
    let (code, stdout, stderr) = cli(&data, server.port(), &["chat-create", "bob"], None).await;
    assert_ok(code, &stdout, &stderr, "chat-create");
    let chat_id = stdout
        .lines()
        .next()
        .expect("первая строка вывода")
        .rsplit(": ")
        .next()
        .expect("chat_id")
        .trim()
        .to_string();
    let secret = "между профилями 468";
    let (code, stdout, stderr) = cli(&data, server.port(), &["send", &chat_id, secret], None).await;
    assert_ok(code, &stdout, &stderr, "send from default");
    let (code, stdout, stderr) =
        cli(&data, server.port(), &["--account", "bob", "sync"], None).await;
    assert_ok(code, &stdout, &stderr, "sync bob profile");
    let (code, stdout, stderr) = cli(
        &data,
        server.port(),
        &["--account", "bob", "history", &chat_id],
        None,
    )
    .await;
    assert_ok(code, &stdout, &stderr, "history bob profile");
    assert!(stdout.contains(secret), "stdout: {stdout}");

    // Активный профиль локально удалить нельзя.
    let (code, _stdout, stderr) = cli(
        &data,
        server.port(),
        &["--account", "bob", "account-delete", "bob", "--yes"],
        None,
    )
    .await;
    assert_ne!(code, 0, "нельзя удалить активный профиль");
    assert!(stderr.contains("текущим"), "stderr: {stderr}");

    // Отказ от подтверждения оставляет профиль на месте.
    let (code, stdout, stderr) = cli(
        &data,
        server.port(),
        &["account-delete", "bob"],
        Some("n\n"),
    )
    .await;
    assert_ok(code, &stdout, &stderr, "account-delete cancel");
    assert!(stdout.contains("Отменено"), "stdout: {stdout}");
    assert!(data.join("accounts/bob/client.sqlite3").is_file());

    // Подтверждённое удаление убирает только bob, default не затронут.
    let (code, stdout, stderr) = cli(
        &data,
        server.port(),
        &["account-delete", "bob", "--yes"],
        None,
    )
    .await;
    assert_ok(code, &stdout, &stderr, "account-delete bob");
    assert!(
        !data.join("accounts/bob").exists(),
        "каталог профиля удалён"
    );
    assert!(
        data.join("client.sqlite3").is_file(),
        "профиль default не затронут"
    );

    let (code, stdout, stderr) = cli(&data, server.port(), &["accounts"], None).await;
    assert_ok(code, &stdout, &stderr, "accounts after delete");
    assert!(!stdout.contains(" bob —"), "stdout: {stdout}");
}

/// Сигнализация звонка в CLI: `call invite --wait` держит соединение,
/// `watch` показывает входящее событие, второе устройство принимает и
/// завершает звонок; отклонение и запрет звонков в группах.
#[tokio::test(flavor = "multi_thread")]
async fn call_signaling_lifecycle() {
    let server = start_server().await;
    let root = tempfile::tempdir().expect("tempdir");
    let alice_dir = root.path().join("alice");
    let bob_dir = root.path().join("bob");
    let bob2_dir = root.path().join("bob2");

    register(&alice_dir, server.port(), "alice", "pw-alice-1").await;
    register(&bob_dir, server.port(), "bob", "pw-bob-1").await;
    let (code, stdout, stderr) = cli(
        &bob2_dir,
        server.port(),
        &["login", "bob", "--password-stdin"],
        Some("pw-bob-1\n"),
    )
    .await;
    assert_ok(code, &stdout, &stderr, "login bob2");

    let chat_id = create_chat(&alice_dir, server.port(), "bob").await;

    // Bob остаётся online в watch; второе устройство принимает звонок.
    let mut watch = spawn_watch(&bob_dir, server.port()).await;
    let mut watch_out = BufReader::new(watch.stdout.take().expect("watch stdout"));
    read_until_line(&mut watch_out, "Слушаю события", "watch online").await;

    // Приглашение: caller удерживает соединение флагом --wait.
    let mut invite = spawn_call_invite(&alice_dir, server.port(), &chat_id).await;
    let mut invite_out = BufReader::new(invite.stdout.take().expect("invite stdout"));
    read_until_line(&mut invite_out, "Ожидаю ответа", "invite waiting").await;
    let incoming = read_until_line(&mut watch_out, "[звонок] входящий:", "bob invited").await;
    let call_id = call_id_from_line(&incoming);

    let (code, stdout, stderr) = cli(
        &bob2_dir,
        server.port(),
        &["call", "accept", &call_id],
        None,
    )
    .await;
    assert_ok(code, &stdout, &stderr, "call accept");
    assert!(stdout.contains("принят"), "stdout: {stdout}");
    read_until_line(&mut invite_out, "[звонок] принят:", "alice sees accept").await;
    read_until_line(&mut watch_out, "[звонок] принят:", "bob watch sees accept").await;

    // Завершение: caller получает событие и выходит из --wait.
    let (code, stdout, stderr) =
        cli(&bob2_dir, server.port(), &["call", "end", &call_id], None).await;
    assert_ok(code, &stdout, &stderr, "call end");
    read_until_line(&mut invite_out, "[звонок] завершён:", "alice sees end").await;
    let status = invite.wait().await.expect("wait invite");
    assert!(status.success(), "caller exit: {status:?}");
    read_until_line(&mut watch_out, "[звонок] завершён:", "bob watch sees end").await;

    // Отклонение: приглашение и decline вторым устройством.
    let mut invite = spawn_call_invite(&alice_dir, server.port(), &chat_id).await;
    let mut invite_out = BufReader::new(invite.stdout.take().expect("invite stdout 2"));
    read_until_line(&mut invite_out, "Ожидаю ответа", "invite 2 waiting").await;
    let incoming = read_until_line(&mut watch_out, "[звонок] входящий:", "bob invited 2").await;
    let call_id = call_id_from_line(&incoming);
    let (code, stdout, stderr) = cli(
        &bob2_dir,
        server.port(),
        &["call", "decline", &call_id],
        None,
    )
    .await;
    assert_ok(code, &stdout, &stderr, "call decline");
    read_until_line(&mut invite_out, "[звонок] отклонён:", "alice sees decline").await;
    let status = invite.wait().await.expect("wait invite 2");
    assert!(status.success(), "caller exit: {status:?}");

    // Групповой чат: звонки запрещены.
    let (code, stdout, stderr) = cli(
        &alice_dir,
        server.port(),
        &[
            "group-create",
            "Группа",
            "--members",
            "bob",
            "--mode",
            "none",
        ],
        None,
    )
    .await;
    assert_ok(code, &stdout, &stderr, "group-create");
    let group_id = stdout
        .lines()
        .next()
        .expect("group id line")
        .rsplit(": ")
        .next()
        .expect("group id")
        .trim()
        .to_string();
    let (code, _stdout, stderr) = cli(
        &alice_dir,
        server.port(),
        &["call", "invite", &group_id],
        None,
    )
    .await;
    assert_ne!(code, 0, "звонок в группе должен быть запрещён");
    assert!(stderr.contains("только в личных"), "stderr: {stderr}");

    watch.kill().await.expect("kill watch");
    let _ = watch.wait().await;
}

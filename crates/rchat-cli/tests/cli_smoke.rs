//! Smoke-тесты CLI: реальный сервер (in-process, без TLS) и настоящий
//! бинарник `rchat-cli` как отдельные процессы.
//!
//! Проверяются основные пользовательские сценарии: регистрация двух
//! пользователей, личный чат, отправка, синхронизация и история, а также
//! офлайн-очередь с автоматической выгрузкой после переподключения.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use rchat_server::{Argon2idHasher, Server, ServerConfig};
use rchat_server_storage_sqlite::net::{handle_client, ws_config};
use rchat_server_storage_sqlite::SqliteRepo;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::process::Command;

/// Сервер на цикле accept; пока структура жива, порт занят, а БД открыта.
struct TestServer {
    port: u16,
    accept: tokio::task::JoinHandle<()>,
    _dir: Option<tempfile::TempDir>,
}

impl TestServer {
    async fn shutdown(self) {
        self.accept.abort();
        let _ = self.accept.await;
    }
}

/// Поднимает сервер на указанном каталоге БД (для рестартов с той же БД).
async fn start_server_in(db_dir: &Path) -> TestServer {
    let config = ServerConfig::default();
    let repo = SqliteRepo::open(db_dir.join("server.sqlite3")).expect("repo");
    let server = Arc::new(
        Server::start(
            Arc::new(repo),
            config.clone(),
            Arc::new(Argon2idHasher::new(config.argon2).expect("hasher")),
        )
        .await
        .expect("server"),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let ws = ws_config(config.max_frame_bytes);
    let accept = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let server = Arc::clone(&server);
            tokio::spawn(async move {
                let _ = handle_client(stream, None, server, ws).await;
            });
        }
    });
    TestServer {
        port,
        accept,
        _dir: None,
    }
}

/// Сервер со своей временной БД.
async fn start_server() -> TestServer {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = start_server_in(dir.path()).await;
    server._dir = Some(dir);
    server
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

    let stdout = register(&alice_dir, server.port, "alice", "secret-alice").await;
    assert!(stdout.contains("Зарегистрирован"), "stdout: {stdout}");
    register(&bob_dir, server.port, "bob", "secret-bob").await;

    let chat_id = create_chat(&alice_dir, server.port, "bob").await;
    assert!(!chat_id.is_empty());

    // Отправка с подтверждением сервера.
    let (code, stdout, stderr) = cli(
        &alice_dir,
        server.port,
        &["send", &chat_id, "Привет, bob!"],
        None,
    )
    .await;
    assert_ok(code, &stdout, &stderr, "send alice");
    assert!(stdout.contains("Отправлено."), "stdout: {stdout}");

    // Второй клиент получает сообщение синхронизацией.
    let (code, stdout, stderr) = cli(&bob_dir, server.port, &["sync"], None).await;
    assert_ok(code, &stdout, &stderr, "sync bob");
    assert!(stdout.contains("применено 2"), "stdout: {stdout}");

    let (code, stdout, stderr) = cli(&bob_dir, server.port, &["history", &chat_id], None).await;
    assert_ok(code, &stdout, &stderr, "history bob");
    assert!(stdout.contains("Привет, bob!"), "stdout: {stdout}");

    // Ответ и повторная синхронизация первого клиента.
    let (code, stdout, stderr) = cli(
        &bob_dir,
        server.port,
        &["send", &chat_id, "И тебе привет"],
        None,
    )
    .await;
    assert_ok(code, &stdout, &stderr, "send bob");

    let (code, stdout, stderr) = cli(&alice_dir, server.port, &["sync"], None).await;
    assert_ok(code, &stdout, &stderr, "sync alice");

    let (code, stdout, stderr) = cli(&alice_dir, server.port, &["history", &chat_id], None).await;
    assert_ok(code, &stdout, &stderr, "history alice");
    assert!(stdout.contains("И тебе привет"), "stdout: {stdout}");

    // Список чатов берётся из локального кеша.
    let (code, stdout, stderr) = cli(&alice_dir, server.port, &["chats"], None).await;
    assert_ok(code, &stdout, &stderr, "chats alice");
    assert!(stdout.contains(&chat_id), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_message_is_flushed_after_reconnect() {
    let root = tempfile::tempdir().expect("tempdir");
    let alice_dir = root.path().join("alice");
    let bob_dir = root.path().join("bob");

    // Первый запуск сервера: регистрация и общий чат.
    let first = start_server_in(root.path()).await;
    register(&alice_dir, first.port, "alice", "secret-alice").await;
    register(&bob_dir, first.port, "bob", "secret-bob").await;
    let chat_id = create_chat(&alice_dir, first.port, "bob").await;
    let first_port = first.port;
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
    let (code, stdout, stderr) = cli(&alice_dir, second.port, &["sync"], None).await;
    assert_ok(code, &stdout, &stderr, "sync after reconnect");
    assert!(
        stdout.contains("Очередь отправки пуста."),
        "stdout: {stdout}"
    );

    let (code, stdout, stderr) = cli(&alice_dir, second.port, &["history", &chat_id], None).await;
    assert_ok(code, &stdout, &stderr, "history after flush");
    assert!(stdout.contains("Сообщение из офлайна"), "stdout: {stdout}");
    assert!(stdout.contains("[отправлено]"), "stdout: {stdout}");
    assert!(!stdout.contains("[в очереди]"), "stdout: {stdout}");

    // Второй клиент видит доставленное сообщение.
    let (code, stdout, stderr) = cli(&bob_dir, second.port, &["sync"], None).await;
    assert_ok(code, &stdout, &stderr, "sync bob");
    let (code, stdout, stderr) = cli(&bob_dir, second.port, &["history", &chat_id], None).await;
    assert_ok(code, &stdout, &stderr, "history bob");
    assert!(stdout.contains("Сообщение из офлайна"), "stdout: {stdout}");
}

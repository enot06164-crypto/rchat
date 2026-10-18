//! Smoke-тест реального бинарника сервера: запуск процесса, WebSocket без
//! TLS на loopback, два клиента: от ClientHello до сообщения, синхронизации
//! и истории.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rchat_proto::proto;
use rchat_proto::proto::envelope;
use rchat_proto::proto::request::Method;
use rchat_proto::proto::response::Result as R;
use rchat_proto::proto::sync_event::Payload;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Убивает процесс сервера при выходе из теста (в том числе при панике).
struct ServerProc(Child);

impl Drop for ServerProc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn server_bin() -> &'static str {
    env!("CARGO_BIN_EXE_rchat-server")
}

/// Свободный порт на loopback.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe");
    listener.local_addr().expect("addr").port()
}

async fn wait_ready(addr: &str) {
    for _ in 0..200 {
        if TcpStream::connect(addr).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("server did not start listening on {addr}");
}

async fn send_envelope(ws: &mut Ws, envelope: proto::Envelope) {
    let frame = rchat_proto::encode_message(&envelope).expect("encode");
    ws.send(Message::Binary(frame.into())).await.expect("send");
}

async fn read_envelope(ws: &mut Ws) -> proto::Envelope {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("frame timeout")
            .expect("stream closed")
            .expect("ws error");
        match frame {
            Message::Binary(data) => {
                return rchat_proto::decode_envelope(&data, 1 << 20).expect("decode")
            }
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("unexpected frame: {other:?}"),
        }
    }
}

async fn request(ws: &mut Ws, request_id: u64, method: Method) -> R {
    send_envelope(
        ws,
        proto::Envelope {
            request_id,
            body: Some(envelope::Body::Request(proto::Request {
                method: Some(method),
            })),
        },
    )
    .await;
    // Пока ждём ответ, соединение может получить push (presence и т.п.).
    loop {
        let response = read_envelope(ws).await;
        if response.request_id != request_id {
            continue;
        }
        match response.body {
            Some(envelope::Body::Response(response)) => {
                return response.result.expect("result");
            }
            other => panic!("expected response, got {other:?}"),
        }
    }
}

/// Подключается и выполняет handshake; возвращает готовое соединение.
async fn connect(addr: &str) -> Ws {
    let (mut ws, _) = connect_async(format!("ws://{addr}"))
        .await
        .expect("ws connect");
    send_envelope(
        &mut ws,
        proto::Envelope {
            request_id: 0,
            body: Some(envelope::Body::ClientHello(proto::ClientHello {
                min_major: 1,
                max_major: 1,
                min_minor: 0,
                max_minor: 1,
                capabilities: Vec::new(),
                required_capabilities: Vec::new(),
                max_frame_bytes: 0,
                client_name: "smoke".to_string(),
                client_version: "0.1".to_string(),
            })),
        },
    )
    .await;
    let hello = read_envelope(&mut ws).await;
    match hello.body {
        Some(envelope::Body::ServerHello(hello)) => {
            assert_eq!(hello.protocol_major, 1);
            assert_eq!(hello.e2e_protocol, "olm.v1");
            assert!(hello.max_frame_bytes >= 4096);
        }
        other => panic!("expected ServerHello, got {other:?}"),
    }
    ws
}

fn device(id: &str) -> proto::DeviceInfo {
    proto::DeviceInfo {
        device_id: id.to_string(),
        name: format!("device-{id}"),
        platform: "smoke".to_string(),
        keys: None,
        e2e_version: 0,
        created_at_ms: 0,
        last_seen_at_ms: 0,
        revoked_at_ms: 0,
        user_id: String::new(),
    }
}

fn register(username: &str, password: &str, device_id: &str) -> Method {
    Method::Register(proto::RegisterRequest {
        username: username.to_string(),
        display_name: username.to_string(),
        password: password.to_string(),
        device: Some(device(device_id)),
        one_time_keys: Vec::new(),
        fallback_key: None,
    })
}

fn spawn_server(addr: &str, db: &std::path::Path) -> ServerProc {
    let child = Command::new(server_bin())
        .args([
            "--listen",
            addr,
            "--db",
            db.to_str().expect("db path"),
            "--plaintext-insecure",
            "--allow-dev-plaintext-chats",
            "--server-name",
            "smoke-server",
        ])
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server binary");
    ServerProc(child)
}

#[tokio::test]
async fn websocket_plaintext_end_to_end() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let dir = tempfile::tempdir().expect("tempdir");
        let port = free_port();
        let addr = format!("127.0.0.1:{port}");
        let _server = spawn_server(&addr, &dir.path().join("server.sqlite3"));
        wait_ready(&addr).await;

        // Два клиента: одно соединение — одна authenticated-личность.
        let mut alice = connect(&addr).await;
        let mut bob = connect(&addr).await;

        let alice_id = match request(&mut alice, 1, register("alice", "password-1", "a1")).await {
            R::Register(response) => response.user_id,
            other => panic!("register alice failed: {other:?}"),
        };
        match request(&mut bob, 1, register("bob", "password-2", "b1")).await {
            R::Register(_) => {}
            other => panic!("register bob failed: {other:?}"),
        }

        // Неизвестный метод: ветвь oneof пуста → unsupported method.
        send_envelope(
            &mut alice,
            proto::Envelope {
                request_id: 2,
                body: Some(envelope::Body::Request(proto::Request { method: None })),
            },
        )
        .await;
        let response = read_envelope(&mut alice).await;
        assert_eq!(response.request_id, 2);
        match response.body {
            Some(envelope::Body::Response(response)) => match response.result {
                Some(R::Error(error)) => {
                    assert_eq!(error.code, proto::ErrorCode::UnsupportedMethod as i32);
                }
                other => panic!("expected unsupported method error, got {other:?}"),
            },
            other => panic!("expected response, got {other:?}"),
        }

        // Чат и сообщение (явный dev-режим без E2E).
        let chat_id = match request(
            &mut alice,
            3,
            Method::CreateDirectChat(proto::CreateDirectChatRequest {
                peer_username: "bob".to_string(),
                e2e_mode: proto::E2eMode::NoneDev as i32,
            }),
        )
        .await
        {
            R::CreateDirectChat(response) => response.chat.expect("chat").chat_id,
            other => panic!("create chat failed: {other:?}"),
        };
        let sent = match request(
            &mut alice,
            4,
            Method::SendMessage(proto::SendMessageRequest {
                chat_id: chat_id.clone(),
                client_message_id: "m1".to_string(),
                content_kind: "text".to_string(),
                reply_to_server_id: 0,
                recipients: Vec::new(),
                plaintext_dev: "привет из smoke-теста".to_string(),
                ..Default::default()
            }),
        )
        .await
        {
            R::SendMessage(response) => response.message.expect("message"),
            other => panic!("send failed: {other:?}"),
        };
        assert_eq!(sent.server_id, 1);
        assert_eq!(sent.sender_user_id, alice_id);

        // Получатель синхронизирует журнал: chat_created + message.
        match request(
            &mut bob,
            2,
            Method::Sync(proto::SyncRequest {
                after_seq: 0,
                limit: 0,
            }),
        )
        .await
        {
            R::Sync(page) => {
                assert_eq!(page.up_to_seq, 2);
                assert!(!page.resync_required);
                assert!(matches!(
                    page.events[0].payload.as_ref(),
                    Some(Payload::ChatCreated(_))
                ));
                let message = page
                    .events
                    .iter()
                    .find_map(|event| match event.payload.as_ref() {
                        Some(Payload::Message(message)) => message.message.clone(),
                        _ => None,
                    })
                    .expect("message event");
                assert_eq!(message.plaintext_dev, "привет из smoke-теста");
            }
            other => panic!("sync failed: {other:?}"),
        }

        // История у получателя.
        match request(
            &mut bob,
            3,
            Method::GetHistory(proto::GetHistoryRequest {
                chat_id,
                before_server_id: 0,
                limit: 10,
            }),
        )
        .await
        {
            R::GetHistory(response) => {
                assert_eq!(response.messages.len(), 1);
                assert_eq!(response.messages[0].plaintext_dev, "привет из smoke-теста");
            }
            other => panic!("history failed: {other:?}"),
        }

        // Корректное закрытие.
        let _ = alice.close(None).await;
        let _ = bob.close(None).await;
    })
    .await
    .expect("smoke test timed out");
}

/// Читает следующий presence-push (прочие кадры пропускает).
async fn read_presence(ws: &mut Ws) -> proto::PresenceEvent {
    loop {
        let envelope = read_envelope(ws).await;
        if let Some(envelope::Body::Push(push)) = envelope.body {
            if let Some(proto::push::Event::Transient(transient)) = push.event {
                if let Some(proto::transient_event::Event::Presence(event)) = transient.event {
                    return event;
                }
            }
        }
    }
}

/// Presence в реальном WebSocket: online при подключении, offline при
/// разрыве и срез состояния для нового соединения (иначе клиент не узнал бы,
/// кто был в сети до его подключения).
#[tokio::test]
async fn websocket_presence_online_offline_and_snapshot() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let dir = tempfile::tempdir().expect("tempdir");
        let port = free_port();
        let addr = format!("127.0.0.1:{port}");
        let _server = spawn_server(&addr, &dir.path().join("server.sqlite3"));
        wait_ready(&addr).await;

        let mut alice = connect(&addr).await;
        let mut bob = connect(&addr).await;
        let alice_id = match request(&mut alice, 1, register("alice", "password-1", "a1")).await {
            R::Register(response) => response.user_id,
            other => panic!("register alice failed: {other:?}"),
        };
        let bob_id = match request(&mut bob, 1, register("bob", "password-2", "b1")).await {
            R::Register(response) => response.user_id,
            other => panic!("register bob failed: {other:?}"),
        };

        // Общий чат делает их собеседниками: только им шлётся presence.
        match request(
            &mut alice,
            2,
            Method::CreateDirectChat(proto::CreateDirectChatRequest {
                peer_username: "bob".to_string(),
                e2e_mode: proto::E2eMode::NoneDev as i32,
            }),
        )
        .await
        {
            R::CreateDirectChat(_) => {}
            other => panic!("create chat failed: {other:?}"),
        }

        // Bob уходит офлайн — alice получает событие.
        let _ = bob.close(None).await;
        drop(bob);
        let offline = read_presence(&mut alice).await;
        assert_eq!(offline.user_id, bob_id);
        assert!(!offline.online);

        // Bob возвращается: alice видит online, а bob получает срез —
        // alice была в сети ещё до его подключения.
        let mut bob = connect(&addr).await;
        match request(
            &mut bob,
            2,
            Method::Login(proto::LoginRequest {
                username: "bob".to_string(),
                password: "password-2".to_string(),
                device: Some(device("b1")),
                one_time_keys: Vec::new(),
                fallback_key: None,
            }),
        )
        .await
        {
            R::Login(_) => {}
            other => panic!("login bob failed: {other:?}"),
        }
        let online = read_presence(&mut alice).await;
        assert_eq!(online.user_id, bob_id);
        assert!(online.online);
        let snapshot = read_presence(&mut bob).await;
        assert_eq!(snapshot.user_id, alice_id);
        assert!(snapshot.online);

        let _ = alice.close(None).await;
        let _ = bob.close(None).await;
    })
    .await
    .expect("presence test timed out");
}

#[test]
fn plaintext_refused_outside_loopback() {
    let output = Command::new(server_bin())
        .args(["--listen", "0.0.0.0:0", "--plaintext-insecure"])
        .output()
        .expect("run server binary");
    assert!(
        !output.status.success(),
        "небезопасный режим должен отвергаться"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("loopback"),
        "ожидалось сообщение про loopback, получено: {stderr}"
    );
}

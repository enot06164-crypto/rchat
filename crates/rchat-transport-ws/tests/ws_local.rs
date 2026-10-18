//! Проверки WebSocket-транспорта на локальном сервере (без TLS).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rchat_core::traits::TransportFactory;
use rchat_transport_ws::{WsTransportConfig, WsTransportFactory};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// Эхо-сервер: возвращает бинарные кадры, считает heartbeat-пинги,
/// завершается по закрытию соединения.
async fn spawn_echo_server() -> (u16, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let pings = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&pings);
    let handle = tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        while let Some(Ok(frame)) = ws.next().await {
            match frame {
                Message::Binary(payload) => {
                    if ws.send(Message::binary(payload)).await.is_err() {
                        break;
                    }
                }
                Message::Ping(_) => {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });
    (port, pings, handle)
}

fn factory(port: u16, heartbeat_ms: u64, read_timeout_ms: u64) -> WsTransportFactory {
    WsTransportFactory::new(WsTransportConfig {
        url: format!("ws://127.0.0.1:{port}"),
        heartbeat: Duration::from_millis(heartbeat_ms),
        read_timeout: Duration::from_millis(read_timeout_ms),
        ..Default::default()
    })
    .expect("factory")
}

#[tokio::test]
async fn echo_heartbeat_and_close() {
    let (port, pings, server) = spawn_echo_server().await;
    let factory = factory(port, 100, 5_000);
    assert_eq!(factory.endpoint().port, port);
    let mut transport = factory.connect().await.expect("connect");

    transport.send(b"hello").await.expect("send");
    let frame = tokio::time::timeout(Duration::from_secs(3), transport.recv())
        .await
        .expect("recv timeout")
        .expect("recv ok")
        .expect("frame");
    assert_eq!(frame, b"hello");

    // Пока клиент ждёт кадры, транспорт обязан отправлять heartbeat.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while pings.load(Ordering::SeqCst) < 2 && tokio::time::Instant::now() < deadline {
        let _ = tokio::time::timeout(Duration::from_millis(150), transport.recv()).await;
    }
    assert!(
        pings.load(Ordering::SeqCst) >= 2,
        "сервер должен получить не менее двух heartbeat-пингов"
    );

    transport.close().await.expect("close");
    // Повторное закрытие идемпотентно.
    transport.close().await.expect("close repeat");
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .expect("server stopped")
        .expect("server task");
    // После закрытия отправка невозможна.
    assert!(transport.send(b"late").await.is_err());
}

#[tokio::test]
async fn server_close_maps_to_none() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let mut ws = tokio_tungstenite::accept_async(stream).await.expect("ws");
        let _ = ws.close(None).await;
    });

    let mut transport = factory(port, 5_000, 6_000)
        .connect()
        .await
        .expect("connect");
    let frame = tokio::time::timeout(Duration::from_secs(5), transport.recv())
        .await
        .expect("recv timeout")
        .expect("recv ok");
    assert!(frame.is_none(), "корректное закрытие — Ok(None)");
    // Дальнейшие чтения возвращают ошибку закрытого соединения.
    assert!(transport.recv().await.is_err());
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .expect("server stopped")
        .expect("server task");
}

#[tokio::test]
async fn oversized_frame_is_rejected() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let mut ws = tokio_tungstenite::accept_async(stream).await.expect("ws");
        let _ = ws.send(Message::binary(vec![0u8; 512])).await;
        let _ = ws.next().await;
    });

    let config = WsTransportConfig {
        url: format!("ws://127.0.0.1:{port}"),
        heartbeat: Duration::from_secs(5),
        read_timeout: Duration::from_secs(6),
        max_frame_bytes: 64,
        ..Default::default()
    };
    let mut transport = WsTransportFactory::new(config)
        .expect("factory")
        .connect()
        .await
        .expect("connect");
    let error = tokio::time::timeout(Duration::from_secs(5), transport.recv())
        .await
        .expect("recv timeout")
        .expect_err("кадр сверх лимита должен быть отклонён");
    assert_eq!(error.kind, rchat_core::error::TransportErrorKind::Frame);
    transport.close().await.expect("close");
    let _ = tokio::time::timeout(Duration::from_secs(3), server).await;
}

#[tokio::test]
async fn read_timeout_reports_lost_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        // Молчим, не отвечаем на ping'и до самого закрытия.
        let _ = ws.next().await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let mut transport = factory(port, 50, 250).connect().await.expect("connect");
    let error = tokio::time::timeout(Duration::from_secs(5), transport.recv())
        .await
        .expect("recv timeout")
        .expect_err("молчащий сервер должен приводить к тайм-ауту");
    assert_eq!(error.kind, rchat_core::error::TransportErrorKind::Timeout);
    server.abort();
}

#[test]
fn invalid_configs_rejected() {
    let bad_url = WsTransportConfig {
        url: "http://127.0.0.1:1".to_string(),
        ..Default::default()
    };
    assert!(WsTransportFactory::new(bad_url).is_err());

    let zero_heartbeat = WsTransportConfig {
        heartbeat: Duration::ZERO,
        ..Default::default()
    };
    assert!(WsTransportFactory::new(zero_heartbeat).is_err());

    let read_timeout_below_heartbeat = WsTransportConfig {
        heartbeat: Duration::from_secs(10),
        read_timeout: Duration::from_secs(10),
        ..Default::default()
    };
    assert!(WsTransportFactory::new(read_timeout_below_heartbeat).is_err());
}

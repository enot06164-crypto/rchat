//! Gateway: приём соединения, handshake, диспетчеризация запросов,
//! push-доставка и presence.
//!
//! Транспорт абстрагирован трейтом `ServerConn`; WebSocket/TLS-реализация
//! живёт в бинарнике сервера, тесты используют in-memory соединения.

use std::sync::Arc;

use rchat_proto::proto;
use rchat_proto::proto::envelope;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::delivery::ConnId;
use crate::error::ServerError;
use crate::proto_util as p;
use crate::service::{ConnState, PeerInfo, Server};

/// Ошибка на уровне соединения.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ConnError {
    #[error("connection I/O error: {0}")]
    Io(String),
    #[error("connection protocol error: {0}")]
    Protocol(String),
}

/// Что пришло от транспорта.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnFrame {
    /// Кадр протокола (бинарное сообщение WebSocket).
    Data(Vec<u8>),
    /// Транспортный heartbeat (ping/pong); продлевает простой, но не является кадром.
    KeepAlive,
    /// Соединение корректно закрыто клиентом.
    Closed,
}

/// Транспорт одного клиентского соединения (серверная сторона).
#[async_trait::async_trait]
pub trait ServerConn: Send {
    /// Следующее событие транспорта.
    async fn recv(&mut self) -> Result<ConnFrame, ConnError>;

    /// Отправляет кадр.
    async fn send(&mut self, frame: &[u8]) -> Result<(), ConnError>;

    /// Закрывает соединение (best-effort).
    async fn close(&mut self, reason: &str) -> Result<(), ConnError>;
}

/// Возможности, поддерживаемые сервером.
pub const SERVER_CAPABILITIES: &[&str] = &[
    rchat_proto::caps::RECEIPTS,
    rchat_proto::caps::PRESENCE,
    rchat_proto::caps::EDIT,
    rchat_proto::caps::DELETE,
    rchat_proto::caps::MULTI_DEVICE,
    rchat_proto::caps::HISTORY,
    rchat_proto::caps::SNAPSHOT,
    rchat_proto::caps::E2E_OLM_V1,
];

/// Результат согласования протокола.
#[derive(Debug, Clone)]
pub struct Negotiated {
    pub protocol_minor: u32,
    pub capabilities: Vec<String>,
    pub max_frame_bytes: u32,
}

/// Внутреннее событие цикла соединения.
enum Event {
    Frame(Vec<u8>),
    KeepAlive,
    Push(Option<Vec<u8>>),
    Idle,
    Closed,
    Failed(ConnError),
}

/// Обслуживает одно соединение от handshake до закрытия.
pub async fn run_connection(server: Arc<Server>, mut conn: Box<dyn ServerConn>, peer: PeerInfo) {
    let config = server.config().clone();
    let (negotiated, mut conn_state) = match handshake(&server, &mut conn, peer.clone()).await {
        Ok(state) => state,
        Err(error) => {
            tracing::info!(peer = %peer.addr, code = ?error.code, "handshake failed");
            if let Ok(frame) = rchat_proto::encode_message(&p::error_envelope(0, &error)) {
                let _ = conn.send(&frame).await;
            }
            let _ = conn.close("handshake failed").await;
            return;
        }
    };
    tracing::info!(
        peer = %peer.addr,
        minor = negotiated.protocol_minor,
        capabilities = negotiated.capabilities.len(),
        "connection established"
    );

    let mut push_rx: Option<mpsc::Receiver<Vec<u8>>> = None;
    let mut conn_id: Option<ConnId> = None;
    let mut registered: Option<(String, String)> = None;
    let mut deadline = Instant::now() + config.conn_idle_timeout();

    loop {
        let event = tokio::select! {
            _ = tokio::time::sleep_until(deadline) => Event::Idle,
            push = recv_push(&mut push_rx) => Event::Push(push),
            frame = conn.recv() => match frame {
                Ok(ConnFrame::Data(frame)) => Event::Frame(frame),
                Ok(ConnFrame::KeepAlive) => Event::KeepAlive,
                Ok(ConnFrame::Closed) => Event::Closed,
                Err(error) => Event::Failed(error),
            },
        };

        let outcome: Result<(), &'static str> = match event {
            Event::Idle => {
                tracing::info!(peer = %peer.addr, "closing idle connection");
                let _ = conn.close("idle timeout").await;
                Err("idle")
            }
            Event::KeepAlive => {
                deadline = Instant::now() + config.conn_idle_timeout();
                Ok(())
            }
            Event::Closed => Err("closed"),
            Event::Failed(error) => {
                tracing::debug!(peer = %peer.addr, error = %error, "connection read failed");
                Err("read failed")
            }
            Event::Push(push) => match push {
                Some(frame) => {
                    if let Err(error) = conn.send(&frame).await {
                        tracing::debug!(error = %error, "push send failed");
                        Err("push send failed")
                    } else {
                        Ok(())
                    }
                }
                None => Err("push channel closed"),
            },
            Event::Frame(frame) => {
                deadline = Instant::now() + config.conn_idle_timeout();
                match handle_frame(&server, &mut conn_state, &frame, negotiated.max_frame_bytes)
                    .await
                {
                    Ok(Some(response)) => {
                        if let Err(error) = conn.send(&response).await {
                            tracing::debug!(error = %error, "response send failed");
                            Err("send failed")
                        } else {
                            Ok(())
                        }
                    }
                    Ok(None) => Ok(()),
                    Err(error) => {
                        tracing::debug!(peer = %peer.addr, code = ?error.code, "frame rejected");
                        if let Ok(frame) =
                            rchat_proto::encode_message(&p::error_envelope(0, &error))
                        {
                            let _ = conn.send(&frame).await;
                        }
                        Err("bad frame")
                    }
                }
            }
        };
        if let Err(_reason) = outcome {
            break;
        }

        // Жизненный цикл аутентификации: push-канал и presence.
        if let Some(auth) = conn_state.auth.as_ref() {
            let key = (auth.user.user_id.clone(), auth.device.device_id.clone());
            if registered.as_ref() != Some(&key) {
                if let Some((old_user, old_device)) = registered.take() {
                    if let Some(id) = conn_id.take() {
                        server.connections().unregister(id);
                    }
                    server
                        .broadcast_presence(&old_user, false, Some(&old_device))
                        .await;
                }
                let (id, rx) = server.connections().register(&key.0, &key.1, 64);
                conn_id = Some(id);
                push_rx = Some(rx);
                registered = Some(key.clone());
                server.broadcast_presence(&key.0, true, Some(&key.1)).await;
                server.send_presence_snapshot(&key.0, &key.1).await;
            }
        } else if conn_state.just_logged_out {
            if let Some((user, device)) = registered.take() {
                if let Some(id) = conn_id.take() {
                    server.connections().unregister(id);
                }
                push_rx = None;
                server.broadcast_presence(&user, false, Some(&device)).await;
            }
        }
        conn_state.just_authenticated = false;
        conn_state.just_logged_out = false;
    }

    // Очистка: соединение и presence.
    if let Some(id) = conn_id.take() {
        server.connections().unregister(id);
    }
    if let Some((user, device)) = registered.take() {
        if !server.connections().is_online(&user) {
            server.broadcast_presence(&user, false, Some(&device)).await;
        }
    }
    let _ = conn.close("connection closed").await;
    tracing::info!(peer = %peer.addr, "connection closed");
}

/// Чтение push-кадра; ожидание «навсегда», пока канал не создан.
async fn recv_push(rx: &mut Option<mpsc::Receiver<Vec<u8>>>) -> Option<Vec<u8>> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending::<Option<Vec<u8>>>().await,
    }
}

/// Handshake: обмен ClientHello/ServerHello и согласование возможностей.
async fn handshake(
    server: &Server,
    conn: &mut Box<dyn ServerConn>,
    peer: PeerInfo,
) -> Result<(Negotiated, ConnState), ServerError> {
    let config = server.config();
    let deadline = tokio::time::Instant::now() + config.hello_timeout();
    let frame = loop {
        let event = tokio::time::timeout_at(deadline, conn.recv())
            .await
            .map_err(|_| ServerError::invalid_request("client hello timeout"))?
            .map_err(|e| ServerError::invalid_request(format!("connection error: {e}")))?;
        match event {
            ConnFrame::Data(frame) => break frame,
            ConnFrame::KeepAlive => continue,
            ConnFrame::Closed => {
                return Err(ServerError::invalid_request(
                    "connection closed before hello",
                ))
            }
        }
    };
    let envelope = rchat_proto::decode_envelope(&frame, config.max_frame_bytes as usize)
        .map_err(|e| ServerError::invalid_request(format!("bad hello frame: {e}")))?;
    let hello = match envelope.body {
        Some(envelope::Body::ClientHello(hello)) => hello,
        _ => {
            return Err(ServerError::invalid_request(
                "the first frame must be ClientHello",
            ))
        }
    };
    if hello.client_name.chars().count() > 64 || hello.client_version.chars().count() > 64 {
        return Err(ServerError::invalid_request("client name/version too long"));
    }
    rchat_proto::validate::capabilities(&hello.capabilities)
        .map_err(|e| ServerError::invalid_request(e.to_string()))?;
    rchat_proto::validate::capabilities(&hello.required_capabilities)
        .map_err(|e| ServerError::invalid_request(e.to_string()))?;

    let major_ok = hello.min_major <= rchat_proto::PROTOCOL_MAJOR
        && hello.max_major >= rchat_proto::PROTOCOL_MAJOR;
    if !major_ok {
        return Err(ServerError::protocol_version_mismatch(format!(
            "server supports protocol major {}",
            rchat_proto::PROTOCOL_MAJOR
        )));
    }
    // Требуемые клиентом возможности должны поддерживаться сервером.
    let unsupported: Vec<&String> = hello
        .required_capabilities
        .iter()
        .filter(|cap| !SERVER_CAPABILITIES.contains(&cap.as_str()))
        .collect();
    if !unsupported.is_empty() {
        let list = unsupported
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ServerError::unsupported_capability(format!(
            "server does not support: {list}"
        )));
    }
    let capabilities: Vec<String> = hello
        .capabilities
        .iter()
        .filter(|cap| SERVER_CAPABILITIES.contains(&cap.as_str()))
        .cloned()
        .collect();
    // Минор согласуется как минимум из возможностей клиента и сервера.
    // Сейчас PROTOCOL_MINOR = 0, поэтому минимум тривиален; allow оставлен,
    // чтобы выражение было готово к росту минорной версии.
    #[allow(clippy::unnecessary_min_or_max)]
    let minor = hello.max_minor.min(rchat_proto::PROTOCOL_MINOR);
    let client_frame_limit = if hello.max_frame_bytes == 0 {
        config.max_frame_bytes
    } else {
        hello.max_frame_bytes
    };
    let max_frame_bytes = client_frame_limit
        .min(config.max_frame_bytes)
        .min(rchat_proto::limits::MAX_FRAME_BYTES_HARD);
    if max_frame_bytes < 4096 {
        return Err(ServerError::invalid_request("max frame size is too small"));
    }
    let negotiated = Negotiated {
        protocol_minor: minor,
        capabilities,
        max_frame_bytes,
    };
    let server_hello = proto::ServerHello {
        protocol_major: rchat_proto::PROTOCOL_MAJOR,
        protocol_minor: minor,
        capabilities: negotiated.capabilities.clone(),
        max_frame_bytes,
        max_message_bytes: config.max_message_bytes,
        max_sync_page: config.max_sync_page,
        max_history_page: config.max_history_page,
        sync_retention_seconds: config.sync_retention_seconds,
        e2e_protocol: rchat_proto::E2E_PROTOCOL_OLM_V1.to_string(),
        server_name: config.server_name.clone(),
        server_version: config.server_version.clone(),
    };
    let frame = rchat_proto::encode_message(&proto::Envelope {
        request_id: 0,
        body: Some(envelope::Body::ServerHello(server_hello)),
    })
    .map_err(|e| ServerError::internal(format!("encode hello: {e}")))?;
    conn.send(&frame)
        .await
        .map_err(|e| ServerError::invalid_request(format!("connection error: {e}")))?;

    Ok((negotiated, ConnState::new(peer)))
}

/// Обрабатывает один кадр клиента. `Ok(Some(frame))` — ответ для отправки.
async fn handle_frame(
    server: &Server,
    state: &mut ConnState,
    frame: &[u8],
    max_frame: u32,
) -> Result<Option<Vec<u8>>, ServerError> {
    let envelope = rchat_proto::decode_envelope(frame, max_frame as usize)
        .map_err(|e| ServerError::invalid_request(format!("bad frame: {e}")))?;
    let request_id = envelope.request_id;
    let body = envelope.body;
    let method = match body {
        Some(envelope::Body::Request(request)) => request.method,
        other => {
            tracing::debug!("unexpected envelope body: {other:?}");
            return Err(ServerError::invalid_request("unexpected frame"));
        }
    };
    let Some(method) = method else {
        // Неизвестная ветвь oneof: метод не поддерживается, соединение живо.
        let error = ServerError::unsupported_method();
        let response = rchat_proto::encode_message(&p::error_envelope(request_id, &error))
            .map_err(|e| ServerError::internal(format!("encode: {e}")))?;
        return Ok(Some(response));
    };
    let result = server.handle_request(state, method).await;
    let response = rchat_proto::encode_message(&proto::Envelope::with_response(request_id, result))
        .map_err(|e| ServerError::internal(format!("encode: {e}")))?;
    Ok(Some(response))
}

//! Реализация `TransportFactory`/`Transport` поверх WebSocket.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use rchat_core::error::{TransportError, TransportErrorKind};
use rchat_core::traits::{Transport, TransportFactory};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Bytes, Error as WsError, Message};
use tokio_tungstenite::{client_async_with_config, WebSocketStream};

use crate::endpoint::Endpoint;
use crate::stream::ClientStream;
use crate::tls::{self, TlsTrust};

/// Конфигурация WebSocket-транспорта.
#[derive(Debug, Clone)]
pub struct WsTransportConfig {
    /// URL сервера: `ws://host:port[/path]` или `wss://host:port[/path]`.
    pub url: String,
    /// Режим доверия к TLS-сертификату (используется только для `wss://`).
    pub tls: TlsTrust,
    /// Интервал между heartbeat-пингами. `Pong` от сервера удерживает
    /// соединение живым; серверный keepalive приходит как `Ping`.
    pub heartbeat: Duration,
    /// Если за это время не пришло ни одного кадра (включая ping/pong),
    /// соединение считается мёртвым. Должно быть больше `heartbeat`.
    pub read_timeout: Duration,
    /// Максимальный размер входящего кадра/сообщения (защита от
    /// некорректного peer'а). Значение по умолчанию — жёсткий лимит
    /// протокола rchat.
    pub max_frame_bytes: usize,
}

impl Default for WsTransportConfig {
    fn default() -> Self {
        Self {
            url: "ws://127.0.0.1:8443".to_string(),
            tls: TlsTrust::SystemRoots,
            heartbeat: Duration::from_secs(20),
            read_timeout: Duration::from_secs(75),
            max_frame_bytes: rchat_proto::limits::MAX_FRAME_BYTES_HARD as usize,
        }
    }
}

/// Фабрика соединений: на каждый цикл реконнекта создаёт новое соединение.
pub struct WsTransportFactory {
    config: WsTransportConfig,
    endpoint: Endpoint,
    tls_config: Option<Arc<rustls::ClientConfig>>,
    ws_config: WebSocketConfig,
}

impl WsTransportFactory {
    /// Проверяет URL и заранее собирает TLS/WebSocket-конфигурации, чтобы
    /// ошибки конфигурации проявлялись до сетевых попыток.
    pub fn new(config: WsTransportConfig) -> Result<Self, TransportError> {
        if config.heartbeat.is_zero() {
            return Err(TransportError::new(
                TransportErrorKind::Other,
                "heartbeat должен быть больше нуля",
            ));
        }
        if config.read_timeout <= config.heartbeat {
            return Err(TransportError::new(
                TransportErrorKind::Other,
                "read_timeout должен быть больше heartbeat",
            ));
        }
        let endpoint = Endpoint::parse(&config.url)?;
        let tls_config = if endpoint.secure {
            Some(Arc::new(tls::client_config(&config.tls)?))
        } else {
            None
        };
        let ws_config = WebSocketConfig::default()
            .max_message_size(Some(config.max_frame_bytes))
            .max_frame_size(Some(config.max_frame_bytes));
        Ok(Self {
            config,
            endpoint,
            tls_config,
            ws_config,
        })
    }

    /// Разобранный адрес сервера (для диагностики и тестов).
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    async fn connect_stream(&self) -> Result<ClientStream, TransportError> {
        let address = (self.endpoint.host.as_str(), self.endpoint.port);
        let tcp = TcpStream::connect(address).await.map_err(|error| {
            TransportError::connect(format!(
                "не удалось подключиться к {}:{}: {error}",
                self.endpoint.host, self.endpoint.port
            ))
        })?;
        let _ = tcp.set_nodelay(true);
        match self.tls_config.as_ref() {
            None => Ok(ClientStream::Plain(tcp)),
            Some(client_config) => {
                let server_name = rustls::pki_types::ServerName::try_from(
                    self.endpoint.host.clone(),
                )
                .map_err(|error| {
                    TransportError::connect(format!(
                        "некорректное имя сервера {:?}: {error}",
                        self.endpoint.host
                    ))
                })?;
                let connector = tokio_rustls::TlsConnector::from(Arc::clone(client_config));
                let stream = connector.connect(server_name, tcp).await.map_err(|error| {
                    TransportError::connect(format!(
                        "ошибка TLS-рукопожатия с {}:{}: {error}",
                        self.endpoint.host, self.endpoint.port
                    ))
                })?;
                Ok(ClientStream::Tls(Box::new(stream)))
            }
        }
    }
}

#[async_trait]
impl TransportFactory for WsTransportFactory {
    async fn connect(&self) -> Result<Box<dyn Transport>, TransportError> {
        let stream = self.connect_stream().await?;
        let (ws, _response) =
            client_async_with_config(self.endpoint.uri.clone(), stream, Some(self.ws_config))
                .await
                .map_err(|error| {
                    TransportError::connect(format!(
                        "WebSocket-handshake с {} не удался: {error}",
                        self.endpoint.uri
                    ))
                })?;
        let now = Instant::now();
        Ok(Box::new(WsTransport {
            ws,
            heartbeat: self.config.heartbeat,
            read_timeout: self.config.read_timeout,
            last_ping: now,
            last_rx: now,
            closed: false,
        }))
    }
}

struct WsTransport {
    ws: WebSocketStream<ClientStream>,
    heartbeat: Duration,
    read_timeout: Duration,
    last_ping: Instant,
    last_rx: Instant,
    closed: bool,
}

/// Результат одного пробуждения в цикле чтения.
enum Wake {
    Frame(Option<Result<Message, WsError>>),
    Heartbeat,
    ReadTimeout,
}

#[async_trait]
impl Transport for WsTransport {
    async fn send(&mut self, frame: &[u8]) -> Result<(), TransportError> {
        if self.closed {
            return Err(TransportError::closed("соединение закрыто"));
        }
        self.ws
            .send(Message::binary(frame.to_vec()))
            .await
            .map_err(|error| map_ws_error(&error))
    }

    async fn recv(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        loop {
            if self.closed {
                return Err(TransportError::closed("соединение закрыто"));
            }
            let ping_at = self.last_ping + self.heartbeat;
            let read_at = self.last_rx + self.read_timeout;
            let wake = tokio::select! {
                biased;
                message = self.ws.next() => Wake::Frame(message),
                _ = tokio::time::sleep_until(ping_at.into()) => Wake::Heartbeat,
                _ = tokio::time::sleep_until(read_at.into()) => Wake::ReadTimeout,
            };
            match wake {
                Wake::Frame(Some(Ok(Message::Binary(payload)))) => {
                    self.last_rx = Instant::now();
                    return Ok(Some(payload.to_vec()));
                }
                Wake::Frame(Some(Ok(Message::Ping(payload)))) => {
                    self.last_rx = Instant::now();
                    self.ws
                        .send(Message::Pong(payload))
                        .await
                        .map_err(|error| map_ws_error(&error))?;
                }
                Wake::Frame(Some(Ok(Message::Pong(_)))) => {
                    self.last_rx = Instant::now();
                }
                Wake::Frame(Some(Ok(Message::Close(_)))) => {
                    self.closed = true;
                    let _ = self.ws.close(None).await;
                    return Ok(None);
                }
                Wake::Frame(Some(Ok(Message::Text(_) | Message::Frame(_)))) => {
                    return Err(TransportError::new(
                        TransportErrorKind::Frame,
                        "сервер прислал текстовый кадр, протокол rchat использует бинарные",
                    ));
                }
                Wake::Frame(Some(Err(error))) => match error {
                    WsError::ConnectionClosed | WsError::AlreadyClosed => {
                        self.closed = true;
                        return Ok(None);
                    }
                    other => return Err(map_ws_error(&other)),
                },
                Wake::Frame(None) => {
                    self.closed = true;
                    return Ok(None);
                }
                Wake::Heartbeat => {
                    self.ws
                        .send(Message::Ping(Bytes::new()))
                        .await
                        .map_err(|error| map_ws_error(&error))?;
                    self.last_ping = Instant::now();
                }
                Wake::ReadTimeout => {
                    self.closed = true;
                    return Err(TransportError::new(
                        TransportErrorKind::Timeout,
                        format!(
                            "от сервера нет кадров дольше {:?}; соединение считается потерянным",
                            self.read_timeout
                        ),
                    ));
                }
            }
        }
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        if !self.closed {
            self.closed = true;
            let _ = self.ws.close(None).await;
        }
        Ok(())
    }
}

fn map_ws_error(error: &WsError) -> TransportError {
    match error {
        WsError::ConnectionClosed | WsError::AlreadyClosed | WsError::Io(_) => {
            TransportError::closed(error.to_string())
        }
        WsError::Protocol(_) | WsError::Utf8(_) | WsError::Capacity(_) => {
            TransportError::new(TransportErrorKind::Frame, error.to_string())
        }
        WsError::WriteBufferFull(_) => TransportError::new(
            TransportErrorKind::Other,
            format!("переполнение буфера WebSocket: {error}"),
        ),
        other => TransportError::new(TransportErrorKind::Other, other.to_string()),
    }
}

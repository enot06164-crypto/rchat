//! Сетевой слой сервера: TLS (rustls) и WebSocket (tokio-tungstenite).
//!
//! Живёт в библиотеке (а не только в бинарнике), чтобы smoke-тесты могли
//! поднимать реальный сервер и проверять кадры протокола.

use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use rchat_server::gateway::{run_connection, ConnError, ConnFrame, ServerConn};
use rchat_server::service::{PeerInfo, Server};
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig as WsConfig;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// Параметры WebSocket-соединения: жёсткие лимиты памяти на кадр и буферы.
pub fn ws_config(max_frame_bytes: u32) -> WsConfig {
    let limit = (max_frame_bytes as usize).saturating_add(4096);
    WsConfig::default()
        .read_buffer_size(16 * 1024)
        .write_buffer_size(0)
        .max_message_size(Some(limit))
        .max_frame_size(Some(limit))
        .accept_unmasked_frames(false)
}

/// Является ли адрес loopback-адресом.
pub fn is_loopback(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_loopback(),
    }
}

/// Собирает `rustls::ServerConfig` из файлов сертификата и ключа.
pub fn tls_from_files(cert: &Path, key: &Path) -> Result<rustls::ServerConfig, String> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
        .map_err(|e| format!("не удалось прочитать сертификат {}: {e}", cert.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("некорректный PEM сертификата {}: {e}", cert.display()))?;
    if certs.is_empty() {
        return Err(format!("в {} нет сертификатов", cert.display()));
    }
    let key = PrivateKeyDer::from_pem_file(key)
        .map_err(|e| format!("не удалось прочитать ключ {}: {e}", key.display()))?;
    rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("некорректная пара сертификат/ключ: {e}"))
}

/// Генерирует самоподписанный сертификат для локальной разработки.
pub fn tls_dev_self_signed(hosts: &[String]) -> Result<rustls::ServerConfig, String> {
    let mut names: Vec<String> = hosts.to_vec();
    if names.is_empty() {
        names.push("localhost".to_string());
    }
    for extra in ["127.0.0.1", "::1"] {
        if !names.iter().any(|name| name == extra) {
            names.push(extra.to_string());
        }
    }
    let certified = rcgen::generate_simple_self_signed(names)
        .map_err(|e| format!("не удалось сгенерировать сертификат: {e}"))?;
    let cert = certified.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        certified.signing_key.serialize_der(),
    ));
    rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .map_err(|e| format!("некорректный самоподписанный сертификат: {e}"))
}

/// Клиентское соединение поверх WebSocket.
struct WsConn<S> {
    ws: WebSocketStream<S>,
}

#[async_trait::async_trait]
impl<S> ServerConn for WsConn<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    async fn recv(&mut self) -> Result<ConnFrame, ConnError> {
        loop {
            match self.ws.next().await {
                None => return Ok(ConnFrame::Closed),
                Some(Err(error)) => return Err(ConnError::Io(error.to_string())),
                Some(Ok(Message::Binary(data))) => return Ok(ConnFrame::Data(data.to_vec())),
                Some(Ok(Message::Ping(payload))) => {
                    // Отвечаем pong и продлеваем простой соединения.
                    self.ws
                        .send(Message::Pong(payload))
                        .await
                        .map_err(|error| ConnError::Io(error.to_string()))?;
                    return Ok(ConnFrame::KeepAlive);
                }
                Some(Ok(Message::Pong(_))) => return Ok(ConnFrame::KeepAlive),
                Some(Ok(Message::Close(_))) => return Ok(ConnFrame::Closed),
                // Текстовые и служебные кадры в протоколе не используются.
                Some(Ok(_)) => continue,
            }
        }
    }

    async fn send(&mut self, frame: &[u8]) -> Result<(), ConnError> {
        self.ws
            .send(Message::Binary(frame.to_vec().into()))
            .await
            .map_err(|error| ConnError::Io(error.to_string()))
    }

    async fn close(&mut self, _reason: &str) -> Result<(), ConnError> {
        let _ = self.ws.close(None).await;
        Ok(())
    }
}

/// Обслуживает принятое WebSocket-соединение.
async fn serve<S>(ws: WebSocketStream<S>, server: Arc<Server>, peer: PeerInfo)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    run_connection(server, Box::new(WsConn { ws }), peer).await;
}

/// Принимает TCP-соединение, при необходимости выполняет TLS-рукопожатие,
/// поднимает WebSocket и передаёт соединение в gateway.
pub async fn handle_client(
    stream: TcpStream,
    tls: Option<tokio_rustls::TlsAcceptor>,
    server: Arc<Server>,
    config: WsConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let remote = stream.peer_addr()?;
    // Ключ лимитов — IP без порта.
    let peer = PeerInfo {
        addr: remote.to_string(),
        rate_key: remote.ip().to_string(),
    };
    match tls {
        Some(acceptor) => {
            let stream = acceptor.accept(stream).await?;
            let ws = tokio_tungstenite::accept_async_with_config(stream, Some(config)).await?;
            serve(ws, server, peer).await;
        }
        None => {
            let ws = tokio_tungstenite::accept_async_with_config(stream, Some(config)).await?;
            serve(ws, server, peer).await;
        }
    }
    Ok(())
}

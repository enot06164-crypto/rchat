//! Проверки TLS-режимов транспорта: пиннинг отпечатка, явный небезопасный
//! режим и отказ при несовпадении отпечатка.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rchat_core::traits::TransportFactory;
use rchat_transport_ws::{
    certificate_fingerprint, TlsTrust, WsTransportConfig, WsTransportFactory,
};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message;

struct TlsServer {
    port: u16,
    fingerprint: [u8; 32],
    handle: tokio::task::JoinHandle<()>,
}

async fn spawn_tls_echo_server() -> TlsServer {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .expect("self-signed cert");
    let cert_der = certified.cert.der().clone();
    let fingerprint = certificate_fingerprint(cert_der.as_ref());
    let key_der = certified.signing_key.serialize_der();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server_config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![cert_der],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der)),
        )
        .expect("server config");
    let acceptor = TlsAcceptor::from(Arc::new(server_config));

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let handle = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(stream).await else {
                    return;
                };
                let Ok(mut ws) = tokio_tungstenite::accept_async(tls).await else {
                    return;
                };
                while let Some(Ok(frame)) = ws.next().await {
                    match frame {
                        Message::Binary(payload) => {
                            if ws.send(Message::binary(payload)).await.is_err() {
                                break;
                            }
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
            });
        }
    });
    TlsServer {
        port,
        fingerprint,
        handle,
    }
}

fn tls_config(port: u16, trust: TlsTrust) -> WsTransportConfig {
    WsTransportConfig {
        url: format!("wss://localhost:{port}"),
        tls: trust,
        heartbeat: Duration::from_secs(5),
        read_timeout: Duration::from_secs(20),
        ..Default::default()
    }
}

#[tokio::test]
async fn fingerprint_pinning_accepts_matching_certificate() {
    let server = spawn_tls_echo_server().await;
    let config = tls_config(server.port, TlsTrust::Fingerprint(server.fingerprint));
    let mut transport = WsTransportFactory::new(config)
        .expect("factory")
        .connect()
        .await
        .expect("connect with pinned fingerprint");
    transport.send(b"tls").await.expect("send");
    let frame = tokio::time::timeout(Duration::from_secs(5), transport.recv())
        .await
        .expect("recv timeout")
        .expect("recv ok")
        .expect("frame");
    assert_eq!(frame, b"tls");
    transport.close().await.expect("close");
    server.handle.abort();
}

#[tokio::test]
async fn fingerprint_pinning_rejects_other_certificate() {
    let server = spawn_tls_echo_server().await;
    let mut wrong = server.fingerprint;
    wrong[0] ^= 0xff;
    let config = tls_config(server.port, TlsTrust::Fingerprint(wrong));
    let result = WsTransportFactory::new(config)
        .expect("factory")
        .connect()
        .await;
    let error = match result {
        Ok(_) => panic!("отпечаток не должен совпасть"),
        Err(error) => error,
    };
    assert_eq!(error.kind, rchat_core::error::TransportErrorKind::Connect);
    server.handle.abort();
}

#[tokio::test]
async fn insecure_mode_accepts_self_signed_certificate() {
    let server = spawn_tls_echo_server().await;
    let config = tls_config(server.port, TlsTrust::Insecure);
    let transport = WsTransportFactory::new(config)
        .expect("factory")
        .connect()
        .await
        .expect("connect");
    drop(transport);
    server.handle.abort();
}

#[tokio::test]
async fn system_roots_reject_self_signed_certificate() {
    let server = spawn_tls_echo_server().await;
    let config = tls_config(server.port, TlsTrust::SystemRoots);
    let result = WsTransportFactory::new(config)
        .expect("factory")
        .connect()
        .await;
    assert!(
        result.is_err(),
        "самоподписанный сертификат не должен приниматься по системным корням"
    );
    server.handle.abort();
}

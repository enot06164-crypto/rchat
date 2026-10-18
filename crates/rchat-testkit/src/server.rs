//! Тестовый сервер: настоящий сервисный слой + SQLite.
//!
//! [`TestServer`] поднимает `rchat-server` (без TLS) на loopback-порту для
//! клиентских integration-тестов CLI и E2E. Тестовые настройки: минимальная
//! стоимость Argon2 (быстрые логины), остальные значения — документированные
//! по умолчанию; конкретный тест донастраивает конфигурацию замыканием.
//!
//! [`ChannelTransportFactory`] даёт соединение к тому же серверу без сети:
//! каналы в памяти + [`run_connection`]. Это «fake server» для сценариев с
//! mock-транспортом: реальный протокол и серверная логика, но без сокетов и
//! без портов.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use rchat_core::error::{TransportError, TransportErrorKind};
use rchat_core::traits::{Transport, TransportFactory};
use rchat_server::config::{Argon2Params, ServerConfig};
use rchat_server::gateway::{run_connection, ConnError, ConnFrame, ServerConn};
use rchat_server::service::PeerInfo;
use rchat_server::{Argon2idHasher, Server};
use rchat_server_storage_sqlite::net::{handle_client, ws_config};
use rchat_server_storage_sqlite::SqliteRepo;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Тестовые параметры Argon2: минимум стоимости (не для продакшена).
pub fn fast_argon2() -> Argon2Params {
    Argon2Params {
        memory_kib: 64,
        iterations: 1,
        parallelism: 1,
    }
}

/// Применяет тестовые значения по умолчанию к конфигурации сервера.
pub fn apply_test_defaults(config: &mut ServerConfig) {
    config.argon2 = fast_argon2();
}

/// Запущенный тестовый сервер.
///
/// Держит временный каталог БД (если он создан самим сервером) и принимающий
/// TCP-цикл. Клиенты подключаются по `ws://127.0.0.1:{port}` или через
/// [`TestServer::channel_factory`].
pub struct TestServer {
    server: Arc<Server>,
    db_path: Option<PathBuf>,
    addr: SocketAddr,
    accept: Option<JoinHandle<()>>,
    _dir: Option<tempfile::TempDir>,
}

impl TestServer {
    /// Сервер с собственной временной БД и тестовыми настройками.
    pub async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    /// Сервер с временной БД; `patch` донастраивает конфигурацию.
    pub async fn start_with(patch: impl FnOnce(&mut ServerConfig)) -> Self {
        let dir = tempfile::tempdir().expect("testkit: tempdir");
        let db_path = dir.path().join("server.sqlite3");
        let repo = SqliteRepo::open(&db_path).expect("testkit: open repo");
        Self::spawn(repo, Some(db_path), Some(dir), patch).await
    }

    /// Сервер на существующем файле БД (перезапуск между тестами: сессии,
    /// сообщения и журнал синхронизации сохраняются).
    pub async fn start_with_db(
        db_path: impl Into<PathBuf>,
        patch: impl FnOnce(&mut ServerConfig),
    ) -> Self {
        let db_path = db_path.into();
        let repo = SqliteRepo::open(&db_path).expect("testkit: open repo");
        Self::spawn(repo, Some(db_path), None, patch).await
    }

    /// Сервер с БД в памяти (без файлов); подходит для канального транспорта.
    pub async fn start_in_memory(patch: impl FnOnce(&mut ServerConfig)) -> Self {
        let repo = SqliteRepo::open_in_memory().expect("testkit: open in-memory repo");
        Self::spawn(repo, None, None, patch).await
    }

    async fn spawn(
        repo: SqliteRepo,
        db_path: Option<PathBuf>,
        dir: Option<tempfile::TempDir>,
        patch: impl FnOnce(&mut ServerConfig),
    ) -> Self {
        let mut config = ServerConfig::default();
        apply_test_defaults(&mut config);
        patch(&mut config);
        let hasher = Argon2idHasher::new(config.argon2).expect("testkit: hasher");
        let server = Arc::new(
            Server::start(Arc::new(repo), config.clone(), Arc::new(hasher))
                .await
                .expect("testkit: server start"),
        );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("testkit: bind");
        let addr = listener.local_addr().expect("testkit: local addr");
        let ws = ws_config(config.max_frame_bytes);
        let accept_server = Arc::clone(&server);
        let accept = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let server = Arc::clone(&accept_server);
                tokio::spawn(async move {
                    let _ = handle_client(stream, None, server, ws).await;
                });
            }
        });
        Self {
            server,
            db_path,
            addr,
            accept: Some(accept),
            _dir: dir,
        }
    }

    /// Порт TCP-приёмника.
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Адрес TCP-приёмника.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// URL для WebSocket-транспорта клиента.
    pub fn ws_url(&self) -> String {
        format!("ws://{}", self.addr)
    }

    /// Путь к файлу БД; паникует, если сервер запущен в памяти.
    pub fn db_path(&self) -> &Path {
        self.db_path
            .as_deref()
            .expect("testkit: server uses in-memory storage")
    }

    /// Путь к файлу БД, если сервер хранит данные на диске.
    pub fn db_path_opt(&self) -> Option<&Path> {
        self.db_path.as_deref()
    }

    /// Сервисный слой сервера (для тестов, работающих через API напрямую).
    pub fn server(&self) -> Arc<Server> {
        Arc::clone(&self.server)
    }

    /// Фабрика соединений через каналы в памяти (без TCP).
    pub fn channel_factory(&self) -> Arc<ChannelTransportFactory> {
        Arc::new(ChannelTransportFactory {
            server: Arc::clone(&self.server),
            connections: AtomicUsize::new(0),
        })
    }

    /// Останавливает приём новых соединений (существующие живут до закрытия).
    pub async fn shutdown(mut self) {
        if let Some(accept) = self.accept.take() {
            accept.abort();
            let _ = accept.await;
        }
    }
}

/// Фабрика клиентского транспорта поверх каналов в памяти.
pub struct ChannelTransportFactory {
    server: Arc<Server>,
    connections: AtomicUsize,
}

#[async_trait]
impl TransportFactory for ChannelTransportFactory {
    async fn connect(&self) -> Result<Box<dyn Transport>, TransportError> {
        let (client_out, server_in) = mpsc::channel(64);
        let (server_out, client_in) = mpsc::channel(64);
        let number = self.connections.fetch_add(1, Ordering::SeqCst) + 1;
        let server = Arc::clone(&self.server);
        let peer = PeerInfo {
            addr: "in-process".to_string(),
            rate_key: format!("in-process:{number}"),
        };
        let conn = ChannelServerConn {
            inbound: server_in,
            outbound: server_out,
        };
        tokio::spawn(async move {
            run_connection(server, Box::new(conn), peer).await;
        });
        Ok(Box::new(ChannelTransport {
            outbound: Some(client_out),
            inbound: client_in,
        }))
    }
}

/// Клиентская сторона канала в память серверу.
pub struct ChannelTransport {
    outbound: Option<mpsc::Sender<Vec<u8>>>,
    inbound: mpsc::Receiver<Vec<u8>>,
}

#[async_trait]
impl Transport for ChannelTransport {
    async fn send(&mut self, frame: &[u8]) -> Result<(), TransportError> {
        let Some(outbound) = self.outbound.as_ref() else {
            return Err(TransportError {
                kind: TransportErrorKind::Closed,
                message: "in-process transport is closed".to_string(),
            });
        };
        outbound
            .send(frame.to_vec())
            .await
            .map_err(|_| TransportError {
                kind: TransportErrorKind::Closed,
                message: "in-process server closed the connection".to_string(),
            })
    }

    async fn recv(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        Ok(self.inbound.recv().await)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        // Разрыв канала: сервер увидит корректное закрытие.
        self.outbound.take();
        Ok(())
    }
}

/// Серверная сторона канала в память клиенту.
struct ChannelServerConn {
    inbound: mpsc::Receiver<Vec<u8>>,
    outbound: mpsc::Sender<Vec<u8>>,
}

#[async_trait]
impl ServerConn for ChannelServerConn {
    async fn recv(&mut self) -> Result<ConnFrame, ConnError> {
        match self.inbound.recv().await {
            Some(frame) => Ok(ConnFrame::Data(frame)),
            None => Ok(ConnFrame::Closed),
        }
    }

    async fn send(&mut self, frame: &[u8]) -> Result<(), ConnError> {
        self.outbound
            .send(frame.to_vec())
            .await
            .map_err(|_| ConnError::Io("in-process client disconnected".to_string()))
    }

    async fn close(&mut self, _reason: &str) -> Result<(), ConnError> {
        Ok(())
    }
}

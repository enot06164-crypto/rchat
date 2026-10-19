//! Конфигурация FFI-клиента и сборка независимого handle.
//!
//! Один handle — один профиль: собственный каталог данных, SQLite, секреты и
//! E2E-состояние. Несколько handle в одном процессе не делят состояние.

use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use rchat_client_storage_sqlite::{ClientSqliteStorage, EncryptedFileSecretStore};
use rchat_core::client::{Client, ClientBuilder};
use rchat_core::config::ClientConfig;
use rchat_crypto_provider::VodozemacCryptoProvider;
use rchat_transport_ws::{TlsTrust, WsTransportConfig, WsTransportFactory};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::event;
use crate::RchatHandle;

/// Ёмкость очереди событий для одного handle. При переполнении события
/// отбрасываются, а хост получает синтетическое событие `event_lagged`.
pub(crate) const EVENT_QUEUE_CAPACITY: usize = 1024;

/// Конфигурация, передаваемая в `rchat_create` как JSON-объект.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FfiConfig {
    /// Каталог профиля (создаётся при необходимости). Один handle — один
    /// каталог; для нескольких аккаунтов создайте несколько handle.
    pub(crate) data_dir: String,
    /// URL сервера: `ws://host:port[/path]` или `wss://host:port[/path]`.
    pub(crate) server: String,
    /// SHA-256 отпечаток TLS-сертификата (hex, разделители допустимы).
    #[serde(default)]
    pub(crate) tls_fingerprint: Option<String>,
    /// Отключить проверку TLS. Только для явного локального тестирования.
    #[serde(default)]
    pub(crate) tls_insecure: bool,
    /// Включить E2E-провайдер (Olm/Megolm). По умолчанию `true`.
    #[serde(default = "default_true")]
    pub(crate) e2e: bool,
    /// Запустить фоновый актор сразу при создании handle. По умолчанию `true`.
    #[serde(default = "default_true")]
    pub(crate) auto_start: bool,
    /// Имя устройства в списке устройств аккаунта.
    #[serde(default)]
    pub(crate) device_name: Option<String>,
    /// Тайм-аут установки соединения/handshake, мс.
    #[serde(default)]
    pub(crate) connect_timeout_ms: Option<u64>,
    /// Тайм-аут одного запроса к серверу, мс.
    #[serde(default)]
    pub(crate) request_timeout_ms: Option<u64>,
}

impl FfiConfig {
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let config: Self =
            serde_json::from_str(text).map_err(|error| format!("config JSON: {error}"))?;
        if config.data_dir.trim().is_empty() {
            return Err("config: data_dir must not be empty".to_string());
        }
        if config.server.trim().is_empty() {
            return Err("config: server must not be empty".to_string());
        }
        if config.tls_insecure && config.tls_fingerprint.is_some() {
            return Err(
                "config: tls_insecure and tls_fingerprint are mutually exclusive".to_string(),
            );
        }
        if let Some(name) = &config.device_name {
            if name.trim().is_empty() {
                return Err("config: device_name must not be empty".to_string());
            }
        }
        for (name, value) in [
            ("connect_timeout_ms", config.connect_timeout_ms),
            ("request_timeout_ms", config.request_timeout_ms),
        ] {
            if let Some(value) = value {
                if value == 0 || value > 600_000 {
                    return Err(format!("config: {name} must be in 1..=600000"));
                }
            }
        }
        Ok(config)
    }
}

/// Собирает клиент и запускает фоновые задачи. Ошибка означает, что handle
/// не создан (C-функция вернёт NULL, текст будет в `rchat_last_error`).
pub(crate) fn build_handle(config: FfiConfig) -> Result<RchatHandle, String> {
    let runtime = crate::runtime()?;
    let data_dir = PathBuf::from(&config.data_dir);
    std::fs::create_dir_all(&data_dir)
        .map_err(|error| format!("data_dir {}: {error}", data_dir.display()))?;
    let data_dir = std::fs::canonicalize(&data_dir).unwrap_or(data_dir);

    let storage = Arc::new(
        ClientSqliteStorage::open(data_dir.join("client.sqlite3"))
            .map_err(|error| format!("client storage: {error}"))?,
    );
    let secrets = Arc::new(
        EncryptedFileSecretStore::open(data_dir.join("secrets.bin"), data_dir.join("secrets.key"))
            .map_err(|error| format!("secret store: {error}"))?,
    );

    let tls = match (&config.tls_fingerprint, config.tls_insecure) {
        (Some(hex), _) => {
            TlsTrust::fingerprint_hex(hex).map_err(|error| format!("tls_fingerprint: {error}"))?
        }
        (None, true) => TlsTrust::Insecure,
        (None, false) => TlsTrust::SystemRoots,
    };
    let transport = WsTransportFactory::new(WsTransportConfig {
        url: config.server.clone(),
        tls,
        ..Default::default()
    })
    .map_err(|error| format!("transport: {error}"))?;

    let device_name = config
        .device_name
        .clone()
        .unwrap_or_else(|| "rchat-ffi".to_string());
    let mut client_config = ClientConfig {
        device_name: device_name.clone(),
        client_name: "rchat-ffi".to_string(),
        e2e_enabled: config.e2e,
        ..Default::default()
    };
    if let Some(ms) = config.connect_timeout_ms {
        client_config.connect_timeout = Duration::from_millis(ms);
    }
    if let Some(ms) = config.request_timeout_ms {
        client_config.request_timeout = Duration::from_millis(ms);
    }

    let mut builder = ClientBuilder::new(
        client_config,
        Arc::new(transport),
        storage.clone(),
        secrets.clone(),
    );
    if config.e2e {
        builder = builder.crypto_provider(Arc::new(VodozemacCryptoProvider::new(
            storage.clone(),
            secrets.clone(),
        )));
    }
    let client: Client = builder
        .build()
        .map_err(|error| format!("client build: {error}"))?;

    // Подписка создаётся до старта, чтобы не потерять ранние события.
    let subscription = client.subscribe();
    let (tx, rx) = mpsc::channel(EVENT_QUEUE_CAPACITY);
    let dropped = Arc::new(AtomicU64::new(0));
    let event_task = runtime.spawn(event::forward(subscription, tx, Arc::clone(&dropped)));

    if config.auto_start {
        if let Err(error) = runtime.block_on(client.start()) {
            event_task.abort();
            return Err(format!("client start: {error}"));
        }
    }

    Ok(RchatHandle {
        client,
        storage,
        data_dir,
        server: config.server,
        e2e: config.e2e,
        device_name,
        events: tokio::sync::Mutex::new(rx),
        dropped_events: dropped,
        event_task: std::sync::Mutex::new(Some(event_task)),
    })
}

fn default_true() -> bool {
    true
}

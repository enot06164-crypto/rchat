//! Конфигурация клиентского ядра.

use std::time::Duration;

use crate::backoff::BackoffConfig;

#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Имя устройства, отображается в списке устройств.
    pub device_name: String,
    pub device_platform: String,
    pub client_name: String,
    pub client_version: String,
    /// Использовать E2E: требовать capability у сервера и CryptoProvider.
    /// При `true` ядро отказывается работать с E2E-чатами без провайдера.
    pub e2e_enabled: bool,
    /// Дополнительные обязательные capabilities (кроме E2E).
    pub required_capabilities: Vec<String>,
    /// Лимит кадра, который готов принять клиент.
    pub max_frame_bytes: u32,
    /// Тайм-аут установки соединения и handshake.
    pub connect_timeout: Duration,
    /// Тайм-аут одного запроса к серверу.
    pub request_timeout: Duration,
    /// Интервал тика обработчика outbox.
    pub outbox_tick: Duration,
    /// Сколько одноразовых prekey держать опубликованными.
    pub prekeys_target: usize,
    /// Ниже этого количества — догрузить до `prekeys_target`.
    pub prekeys_low_watermark: usize,
    /// Задержки повторных попыток.
    pub backoff: BackoffConfig,
    /// Размер страницы sync (0 — серверное значение по умолчанию).
    pub sync_page_limit: u32,
    /// Автоматически подтверждать доставку применённых сообщений.
    pub auto_mark_delivered: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            device_name: "device".to_string(),
            device_platform: std::env::consts::OS.to_string(),
            client_name: "rchat".to_string(),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            e2e_enabled: true,
            required_capabilities: Vec::new(),
            max_frame_bytes: rchat_proto::limits::MAX_FRAME_BYTES_HARD,
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(20),
            outbox_tick: Duration::from_millis(500),
            prekeys_target: 50,
            prekeys_low_watermark: 10,
            backoff: BackoffConfig::default(),
            sync_page_limit: 0,
            auto_mark_delivered: true,
        }
    }
}

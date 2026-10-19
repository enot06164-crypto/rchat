//! Конфигурация сервера и значения по умолчанию.
//!
//! Все лимиты, тайм-ауты и политика хранения журнала синхронизации
//! конфигурируемы; значения ниже — документированные значения по умолчанию.

use std::time::Duration;

/// Параметры Argon2id (документируются в конфигурации сервера).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Argon2Params {
    /// Память в КиБ (по умолчанию 19 МиБ — рекомендация OWASP).
    pub memory_kib: u32,
    /// Число проходов.
    pub iterations: u32,
    /// Степень параллелизма.
    pub parallelism: u32,
}

impl Default for Argon2Params {
    fn default() -> Self {
        Self {
            memory_kib: 19_456,
            iterations: 2,
            parallelism: 1,
        }
    }
}

/// Базовые лимиты частоты операций.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimits {
    /// Регистраций в час с одного сетевого адреса.
    pub register_per_hour: u32,
    /// Попыток входа на username за 5 минут.
    pub login_per_5min: u32,
    /// Отправок сообщений на пользователя в минуту.
    pub send_per_minute: u32,
    /// Приглашений на звонок на пользователя в минуту.
    pub call_invite_per_minute: u32,
}

impl Default for RateLimits {
    fn default() -> Self {
        Self {
            register_per_hour: 10,
            login_per_5min: 10,
            send_per_minute: 120,
            call_invite_per_minute: 30,
        }
    }
}

/// Конфигурация сервера.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub server_name: String,
    pub server_version: String,

    /// Максимальный принимаемый размер кадра.
    pub max_frame_bytes: u32,
    /// Максимальный размер содержимого сообщения (plaintext или ciphertext).
    pub max_message_bytes: u32,
    /// Максимум событий в странице синхронизации.
    pub max_sync_page: u32,
    /// Максимум сообщений в странице истории.
    pub max_history_page: u32,
    /// Сколько секунд хранится событие журнала синхронизации.
    pub sync_retention_seconds: u64,
    /// Сколько последних событий устройства сохраняются всегда.
    pub sync_keep_min_events: u32,

    /// Время жизни сессии (секунды).
    pub session_ttl_seconds: u64,
    /// Как часто обновлять `last_used_at` сессии (секунды).
    pub session_touch_interval_seconds: u64,

    /// Тайм-аут ожидания ClientHello (секунды).
    pub hello_timeout_seconds: u64,
    /// Тайм-аут простоя соединения (секунды); клиент шлёт heartbeat чаще.
    pub conn_idle_timeout_seconds: u64,

    /// Разрешать ли чаты `E2E_MODE_NONE_DEV` (локальная разработка и тесты).
    /// Явно документированный режим: не является защищённым E2E.
    pub allow_dev_plaintext_chats: bool,

    /// Максимум участников группового чата (жёсткий предел протокола
    /// применяется поверх значения конфигурации).
    pub max_group_members: usize,

    pub argon2: Argon2Params,
    pub rate_limits: RateLimits,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            server_name: "rchat-server".to_string(),
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            max_frame_bytes: rchat_proto::limits::MAX_FRAME_BYTES_DEFAULT,
            max_message_bytes: rchat_proto::limits::MAX_MESSAGE_BYTES_DEFAULT,
            max_sync_page: rchat_proto::limits::MAX_SYNC_PAGE_DEFAULT,
            max_history_page: rchat_proto::limits::MAX_HISTORY_PAGE_DEFAULT,
            sync_retention_seconds: 7 * 24 * 60 * 60,
            sync_keep_min_events: 500,
            session_ttl_seconds: 30 * 24 * 60 * 60,
            session_touch_interval_seconds: 60,
            hello_timeout_seconds: 10,
            conn_idle_timeout_seconds: 150,
            allow_dev_plaintext_chats: true,
            max_group_members: rchat_proto::limits::MAX_GROUP_MEMBERS_DEFAULT,
            argon2: Argon2Params::default(),
            rate_limits: RateLimits::default(),
        }
    }
}

impl ServerConfig {
    pub fn hello_timeout(&self) -> Duration {
        Duration::from_secs(self.hello_timeout_seconds)
    }

    pub fn conn_idle_timeout(&self) -> Duration {
        Duration::from_secs(self.conn_idle_timeout_seconds)
    }

    pub fn session_touch_interval(&self) -> Duration {
        Duration::from_secs(self.session_touch_interval_seconds)
    }

    pub fn sync_retention(&self) -> Duration {
        Duration::from_secs(self.sync_retention_seconds)
    }
}

//! WebSocket-транспорт клиента rchat.
//!
//! Крейт реализует контракты `rchat_core::Transport` и
//! `rchat_core::TransportFactory` поверх `tokio-tungstenite`:
//!
//! * `ws://` — TCP без шифрования (только явное локальное тестирование);
//! * `wss://` — TLS с одним из трёх режимов доверия ([`TlsTrust`]):
//!   системные корневые сертификаты (боевой режим), пиннинг SHA-256
//!   отпечатка листового сертификата (локальная разработка и
//!   самоподписанные сертификаты), полное отключение проверки (только для
//!   явно локальных сценариев).
//!
//! Транспорт не знает о содержимом кадров: ядро передаёт ему готовые
//! `Envelope`. Heartbeat (WebSocket ping/pong) и чтение кадров совмещены в
//! одном цикле `Transport::recv`, поэтому ядру не нужно планировать
//! дополнительные таймеры.

mod endpoint;
mod stream;
mod tls;
mod transport;

pub use endpoint::Endpoint;
pub use tls::{certificate_fingerprint, TlsTrust};
pub use transport::{WsTransportConfig, WsTransportFactory};

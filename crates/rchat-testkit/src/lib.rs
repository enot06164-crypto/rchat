//! Testkit: mock-компоненты и тестовый сервер для интеграционных тестов.
//!
//! Крейт подключается только dev-зависимостями (см. `docs/testing.md`);
//! в сборку приложения он не входит. Состав:
//!
//! - [`fixtures`] — детерминированные генераторы доменных записей;
//! - [`mock_storage`] — in-memory реализация `Storage`/`CryptoStateStore`
//!   с инъекцией сбоя транзакции и [`MockSecretStore`];
//! - [`mock_transport`] — обёртка над транспортом с управляемыми помехами
//!   (задержки, разрывы, ошибки, повторы и внедрение кадров);
//! - [`server`] — настоящий сервер (сервисный слой + SQLite) на TCP или на
//!   каналах в памяти, с документированными тестовыми настройками.
//!
//! Тестовой криптографии здесь нет намеренно: логика ядра проверяется с
//! mock-компонентами, но границы и безопасность E2E — только с реальным
//! провайдером (vodozemac) в `rchat-e2e-tests`.

pub mod fixtures;
pub mod mock_storage;
pub mod mock_transport;
pub mod server;

pub use mock_storage::{MockSecretStore, MockStorage};
pub use mock_transport::{FaultHandle, MockTransportFactory};
pub use server::{ChannelTransportFactory, TestServer};

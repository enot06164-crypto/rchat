//! Ядро клиента rchat: протокольная логика, синхронизация, outbox и E2E-модель.
//!
//! Ядро не зависит от конкретных транспорта, хранилища, CLI или
//! криптографической библиотеки: все внешние зависимости описываются
//! трейтами (`traits`) и подключаются приложением. Допустимые зависимости
//! ядра и обоснование выбора — `docs/architecture.md`.

pub mod avatar;
pub mod backoff;
pub mod client;
pub mod config;
pub mod error;
pub mod events;
pub mod payload;
pub mod traits;
pub mod types;

pub(crate) mod actor;
pub(crate) mod convert;
pub(crate) mod send;

pub use client::{Client, ClientBuilder};
pub use config::ClientConfig;
pub use error::{CoreError, CryptoError, SecretStoreError, StorageError, TransportError};
pub use events::{EventError, EventSubscription, UiEvent};
pub use payload::{Attachment, AttachmentKind, Payload, MAX_ATTACHMENT_BYTES};
pub use traits::{
    secret_keys, ClientStore, CryptoProvider, CryptoStateStore, EncryptedMessage, GroupEncryption,
    GroupKeyInfo, KeyUpload, OneTimeKey, PeerDevice, SecretStore, Storage, Transport,
    TransportFactory,
};
pub use types::{
    ChatRecord, ConnectionState, DeviceRecord, LocalAccount, MemberRecord, MessageRecord,
    MessageStatus, OutboxRecord, OutboxState, ReceiptRecord, SyncApply, SyncProgress, UserRecord,
};

//! Сервер rchat: gateway, auth, чаты и сообщения, синхронизация, доставка.
//!
//! Крейт не содержит SQL: хранение изолировано трейтом [`repo::Repo`]
//! (SQLite-реализация — `rchat-server-storage-sqlite`). Сеть (TLS/WebSocket)
//! живёт в бинарнике сервера; gateway работает через [`gateway::ServerConn`].

pub mod config;
pub mod delivery;
pub mod error;
pub mod gateway;
pub mod model;
pub mod password;
pub mod proto_util;
pub mod rate;
pub mod repo;
pub mod service;

pub use config::{Argon2Params, RateLimits, ServerConfig};
pub use delivery::Connections;
pub use error::ServerError;
pub use gateway::{run_connection, ConnError, Negotiated, ServerConn};
pub use model::*;
pub use password::{Argon2idHasher, PasswordError, PasswordHasher};
pub use repo::{Repo, RepoError};
pub use service::{AuthContext, ConnState, PeerInfo, Server};

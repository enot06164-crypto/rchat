//! Реализация [`rchat_core::CryptoStateStore`] поверх SQLite.
//!
//! Ключи провайдера криптографии (olm-аккаунт и сессии) лежат в отдельных
//! строках таблицы `crypto_state`; сами значения шифрует провайдер (ключ
//! шифрования pickles приходит из хранилища секретов).

use async_trait::async_trait;
use rchat_core::error::StorageError;
use rchat_core::traits::CryptoStateStore;
use rusqlite::OptionalExtension;

use crate::helpers::{escape_like, map_sql};
use crate::ClientSqliteStorage;

#[async_trait]
impl CryptoStateStore for ClientSqliteStorage {
    async fn crypto_load(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        let key = key.to_string();
        self.call(move |conn| {
            conn.query_row(
                "SELECT value FROM crypto_state WHERE key = ?1",
                rusqlite::params![key],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(|e| map_sql("crypto_load", e))
        })
        .await
    }

    async fn crypto_save(&self, key: &str, value: &[u8]) -> Result<(), StorageError> {
        let key = key.to_string();
        let value = value.to_vec();
        self.call(move |conn| {
            conn.execute(
                "INSERT INTO crypto_state(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                rusqlite::params![key, value],
            )
            .map_err(|e| map_sql("crypto_save", e))?;
            Ok(())
        })
        .await
    }

    async fn crypto_delete(&self, key: &str) -> Result<(), StorageError> {
        let key = key.to_string();
        self.call(move |conn| {
            conn.execute(
                "DELETE FROM crypto_state WHERE key = ?1",
                rusqlite::params![key],
            )
            .map_err(|e| map_sql("crypto_delete", e))?;
            Ok(())
        })
        .await
    }

    async fn crypto_delete_prefix(&self, prefix: &str) -> Result<u64, StorageError> {
        let pattern = format!("{}%", escape_like(prefix));
        self.call(move |conn| {
            let deleted = conn
                .execute(
                    "DELETE FROM crypto_state WHERE key LIKE ?1 ESCAPE '\\'",
                    rusqlite::params![pattern],
                )
                .map_err(|e| map_sql("crypto_delete_prefix", e))?;
            Ok(deleted as u64)
        })
        .await
    }
}

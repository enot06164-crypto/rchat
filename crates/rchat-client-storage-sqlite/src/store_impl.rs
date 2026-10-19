//! Реализация контракта [`rchat_core::Storage`] поверх SQLite.

use async_trait::async_trait;
use rchat_core::error::StorageError;
use rchat_core::traits::Storage;
use rchat_core::types::{
    ChatRecord, DeviceRecord, LocalAccount, MemberRecord, MessageRecord, MessageStatus,
    OutboxRecord, OutboxState, ReceiptRecord, SyncApply, UserRecord,
};
use rusqlite::{Connection, OptionalExtension, Transaction};

use crate::helpers::{
    chat_from_row, device_from_row, map_sql, member_from_row, message_from_row, opt_to_sql_u64,
    outbox_from_row, receipt_from_row, to_sql_u64, user_from_row, CHAT_COLUMNS, DEVICE_COLUMNS,
    MESSAGE_COLUMNS, OUTBOX_COLUMNS, RECEIPT_COLUMNS,
};
use crate::schema;
use crate::{ClientSqliteStorage, SCHEMA_VERSION};

#[async_trait]
impl Storage for ClientSqliteStorage {
    async fn migrate(&self) -> Result<(), StorageError> {
        self.call(|conn| schema::migrate_to(conn, SCHEMA_VERSION))
            .await
    }

    // --- аккаунт ---

    async fn save_account(&self, account: &LocalAccount) -> Result<(), StorageError> {
        let account = account.clone();
        self.call(move |conn| {
            conn.execute(
                "INSERT INTO account(id, user_id, username, display_name, device_id)
                 VALUES(1, ?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET
                     user_id = excluded.user_id,
                     username = excluded.username,
                     display_name = excluded.display_name,
                     device_id = excluded.device_id",
                rusqlite::params![
                    account.user_id,
                    account.username,
                    account.display_name,
                    account.device_id
                ],
            )
            .map_err(|e| map_sql("save_account", e))?;
            Ok(())
        })
        .await
    }

    async fn load_account(&self) -> Result<Option<LocalAccount>, StorageError> {
        self.call(|conn| {
            conn.query_row(
                "SELECT user_id, username, display_name, device_id FROM account WHERE id = 1",
                [],
                |row| {
                    Ok(LocalAccount {
                        user_id: row.get(0)?,
                        username: row.get(1)?,
                        display_name: row.get(2)?,
                        device_id: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(|e| map_sql("load_account", e))
        })
        .await
    }

    async fn clear_all(&self) -> Result<(), StorageError> {
        self.call(|conn| {
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| map_sql("clear_all begin", e))?;
            tx.execute_batch(
                "DELETE FROM account;
                 DELETE FROM users;
                 DELETE FROM devices;
                 DELETE FROM chats;
                 DELETE FROM chat_members;
                 DELETE FROM group_key_shares;
                 DELETE FROM messages;
                 DELETE FROM outbox;
                 DELETE FROM receipts;
                 DELETE FROM crypto_state;
                 INSERT OR REPLACE INTO sync_state(id, cursor) VALUES(1, 0);",
            )
            .map_err(|e| map_sql("clear_all", e))?;
            tx.commit().map_err(|e| map_sql("clear_all commit", e))?;
            Ok(())
        })
        .await
    }

    // --- пользователи и устройства ---

    async fn upsert_user(&self, user: &UserRecord) -> Result<(), StorageError> {
        let user = user.clone();
        self.call(move |conn| upsert_user_sql(conn, &user)).await
    }

    async fn get_user(&self, user_id: &str) -> Result<Option<UserRecord>, StorageError> {
        let user_id = user_id.to_string();
        self.call(move |conn| {
            conn.query_row(
                "SELECT user_id, username, display_name FROM users WHERE user_id = ?1",
                rusqlite::params![user_id],
                user_from_row,
            )
            .optional()
            .map_err(|e| map_sql("get_user", e))
        })
        .await
    }

    async fn list_users(&self) -> Result<Vec<UserRecord>, StorageError> {
        self.call(|conn| {
            let mut statement = conn
                .prepare("SELECT user_id, username, display_name FROM users ORDER BY username")
                .map_err(|e| map_sql("list_users prepare", e))?;
            let rows = statement
                .query_map([], user_from_row)
                .map_err(|e| map_sql("list_users", e))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| map_sql("list_users row", e))
        })
        .await
    }

    async fn upsert_device(&self, device: &DeviceRecord) -> Result<(), StorageError> {
        let device = device.clone();
        self.call(move |conn| upsert_device_sql(conn, &device))
            .await
    }

    async fn get_device(&self, device_id: &str) -> Result<Option<DeviceRecord>, StorageError> {
        let device_id = device_id.to_string();
        self.call(move |conn| {
            conn.query_row(
                &format!("SELECT {DEVICE_COLUMNS} FROM devices WHERE device_id = ?1"),
                rusqlite::params![device_id],
                device_from_row,
            )
            .optional()
            .map_err(|e| map_sql("get_device", e))
        })
        .await
    }

    async fn list_devices_of(&self, user_id: &str) -> Result<Vec<DeviceRecord>, StorageError> {
        let user_id = user_id.to_string();
        self.call(move |conn| {
            let mut statement = conn
                .prepare(&format!(
                    "SELECT {DEVICE_COLUMNS} FROM devices WHERE user_id = ?1 ORDER BY device_id"
                ))
                .map_err(|e| map_sql("list_devices_of prepare", e))?;
            let rows = statement
                .query_map(rusqlite::params![user_id], device_from_row)
                .map_err(|e| map_sql("list_devices_of", e))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| map_sql("list_devices_of row", e))
        })
        .await
    }

    async fn delete_device(&self, device_id: &str) -> Result<(), StorageError> {
        let device_id = device_id.to_string();
        self.call(move |conn| {
            conn.execute(
                "DELETE FROM devices WHERE device_id = ?1",
                rusqlite::params![device_id],
            )
            .map_err(|e| map_sql("delete_device", e))?;
            Ok(())
        })
        .await
    }

    // --- чаты ---

    async fn upsert_chat(
        &self,
        chat: &ChatRecord,
        members: &[MemberRecord],
    ) -> Result<(), StorageError> {
        let chat = chat.clone();
        let members = members.to_vec();
        self.call(move |conn| {
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| map_sql("upsert_chat begin", e))?;
            upsert_chat_sql(&tx, &chat, &members)?;
            tx.commit().map_err(|e| map_sql("upsert_chat commit", e))?;
            Ok(())
        })
        .await
    }

    async fn replace_chats(
        &self,
        chats: &[(ChatRecord, Vec<MemberRecord>)],
    ) -> Result<(), StorageError> {
        let chats = chats.to_vec();
        self.call(move |conn| {
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| map_sql("replace_chats begin", e))?;
            tx.execute("DELETE FROM chat_members", [])
                .map_err(|e| map_sql("replace_chats members", e))?;
            tx.execute("DELETE FROM chats", [])
                .map_err(|e| map_sql("replace_chats chats", e))?;
            for (chat, members) in &chats {
                upsert_chat_sql(&tx, chat, members)?;
            }
            tx.commit()
                .map_err(|e| map_sql("replace_chats commit", e))?;
            Ok(())
        })
        .await
    }

    async fn list_chats(&self) -> Result<Vec<ChatRecord>, StorageError> {
        self.call(|conn| {
            let mut statement = conn
                .prepare(&format!(
                    "SELECT {CHAT_COLUMNS} FROM chats ORDER BY updated_at_ms DESC, chat_id"
                ))
                .map_err(|e| map_sql("list_chats prepare", e))?;
            let rows = statement
                .query_map([], chat_from_row)
                .map_err(|e| map_sql("list_chats", e))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| map_sql("list_chats row", e))
        })
        .await
    }

    async fn get_chat(&self, chat_id: &str) -> Result<Option<ChatRecord>, StorageError> {
        let chat_id = chat_id.to_string();
        self.call(move |conn| {
            conn.query_row(
                &format!("SELECT {CHAT_COLUMNS} FROM chats WHERE chat_id = ?1"),
                rusqlite::params![chat_id],
                chat_from_row,
            )
            .optional()
            .map_err(|e| map_sql("get_chat", e))
        })
        .await
    }

    async fn chat_members(&self, chat_id: &str) -> Result<Vec<MemberRecord>, StorageError> {
        let chat_id = chat_id.to_string();
        self.call(move |conn| {
            let mut statement = conn
                .prepare(
                    "SELECT user_id, username, display_name, role, joined_at_ms
                     FROM chat_members WHERE chat_id = ?1
                     ORDER BY joined_at_ms, user_id",
                )
                .map_err(|e| map_sql("chat_members prepare", e))?;
            let rows = statement
                .query_map(rusqlite::params![chat_id], member_from_row)
                .map_err(|e| map_sql("chat_members", e))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| map_sql("chat_members row", e))
        })
        .await
    }

    // --- сообщения ---

    async fn get_message(&self, local_id: &str) -> Result<Option<MessageRecord>, StorageError> {
        let local_id = local_id.to_string();
        self.call(move |conn| {
            conn.query_row(
                &format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE local_id = ?1"),
                rusqlite::params![local_id],
                message_from_row,
            )
            .optional()
            .map_err(|e| map_sql("get_message", e))
        })
        .await
    }

    async fn get_message_by_server(
        &self,
        server_id: u64,
    ) -> Result<Option<MessageRecord>, StorageError> {
        let server_id = to_sql_u64("get_message_by_server", server_id)?;
        self.call(move |conn| {
            conn.query_row(
                &format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE server_id = ?1"),
                rusqlite::params![server_id],
                message_from_row,
            )
            .optional()
            .map_err(|e| map_sql("get_message_by_server", e))
        })
        .await
    }

    async fn list_messages(
        &self,
        chat_id: &str,
        before_server_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<MessageRecord>, StorageError> {
        let chat_id = chat_id.to_string();
        let before = opt_to_sql_u64(before_server_id);
        self.call(move |conn| {
            // Локальные сообщения без server_id ещё не подтверждены сервером,
            // поэтому считаются самыми новыми и идут первыми.
            let mut statement = conn
                .prepare(&format!(
                    "SELECT {MESSAGE_COLUMNS} FROM messages
                     WHERE chat_id = ?1
                       AND (?2 IS NULL OR (server_id IS NOT NULL AND server_id < ?2))
                     ORDER BY COALESCE(server_id, 9223372036854775807) DESC, created_at_ms DESC
                     LIMIT ?3"
                ))
                .map_err(|e| map_sql("list_messages prepare", e))?;
            let rows = statement
                .query_map(
                    rusqlite::params![chat_id, before, limit as i64],
                    message_from_row,
                )
                .map_err(|e| map_sql("list_messages", e))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| map_sql("list_messages row", e))
        })
        .await
    }

    // --- outbox ---

    async fn insert_outgoing(
        &self,
        message: &MessageRecord,
        outbox: &OutboxRecord,
    ) -> Result<(), StorageError> {
        let message = message.clone();
        let outbox = outbox.clone();
        self.call(move |conn| {
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| map_sql("insert_outgoing begin", e))?;
            insert_message_sql(&tx, &message)?;
            tx.execute(
                "INSERT INTO outbox(local_id, chat_id, idempotency_key, request, state,
                                    attempts, next_attempt_at_ms, last_error_code, last_error,
                                    created_at_ms, server_message_id)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    outbox.local_id,
                    outbox.chat_id,
                    outbox.idempotency_key,
                    outbox.request,
                    outbox.state.as_i64(),
                    outbox.attempts as i64,
                    to_sql_u64("insert_outgoing next_attempt", outbox.next_attempt_at_ms)?,
                    outbox.last_error_code,
                    outbox.last_error,
                    to_sql_u64("insert_outgoing created", outbox.created_at_ms)?,
                    opt_to_sql_u64(outbox.server_message_id),
                ],
            )
            .map_err(|e| map_sql("insert_outgoing outbox", e))?;
            tx.commit()
                .map_err(|e| map_sql("insert_outgoing commit", e))?;
            Ok(())
        })
        .await
    }

    async fn get_outbox(&self, local_id: &str) -> Result<Option<OutboxRecord>, StorageError> {
        let local_id = local_id.to_string();
        self.call(move |conn| {
            conn.query_row(
                &format!("SELECT {OUTBOX_COLUMNS} FROM outbox WHERE local_id = ?1"),
                rusqlite::params![local_id],
                outbox_from_row,
            )
            .optional()
            .map_err(|e| map_sql("get_outbox", e))
        })
        .await
    }

    async fn update_outbox(&self, outbox: &OutboxRecord) -> Result<(), StorageError> {
        let outbox = outbox.clone();
        self.call(move |conn| {
            conn.execute(
                "UPDATE outbox SET chat_id = ?2, idempotency_key = ?3, request = ?4, state = ?5,
                                   attempts = ?6, next_attempt_at_ms = ?7, last_error_code = ?8,
                                   last_error = ?9, server_message_id = ?10
                 WHERE local_id = ?1",
                rusqlite::params![
                    outbox.local_id,
                    outbox.chat_id,
                    outbox.idempotency_key,
                    outbox.request,
                    outbox.state.as_i64(),
                    outbox.attempts as i64,
                    to_sql_u64("update_outbox next_attempt", outbox.next_attempt_at_ms)?,
                    outbox.last_error_code,
                    outbox.last_error,
                    opt_to_sql_u64(outbox.server_message_id),
                ],
            )
            .map_err(|e| map_sql("update_outbox", e))?;
            Ok(())
        })
        .await
    }

    async fn delete_outbox(&self, local_id: &str) -> Result<(), StorageError> {
        let local_id = local_id.to_string();
        self.call(move |conn| {
            conn.execute(
                "DELETE FROM outbox WHERE local_id = ?1",
                rusqlite::params![local_id],
            )
            .map_err(|e| map_sql("delete_outbox", e))?;
            Ok(())
        })
        .await
    }

    async fn due_outbox(&self, now_ms: u64, limit: u32) -> Result<Vec<OutboxRecord>, StorageError> {
        let now_ms = to_sql_u64("due_outbox", now_ms)?;
        self.call(move |conn| {
            let mut statement = conn
                .prepare(&format!(
                    "SELECT {OUTBOX_COLUMNS} FROM outbox
                     WHERE state <> ?1 AND next_attempt_at_ms <= ?2
                     ORDER BY created_at_ms, local_id
                     LIMIT ?3"
                ))
                .map_err(|e| map_sql("due_outbox prepare", e))?;
            let rows = statement
                .query_map(
                    rusqlite::params![OutboxState::Sent.as_i64(), now_ms, limit as i64],
                    outbox_from_row,
                )
                .map_err(|e| map_sql("due_outbox", e))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| map_sql("due_outbox row", e))
        })
        .await
    }

    async fn reset_inflight_outbox(&self) -> Result<(), StorageError> {
        self.call(|conn| {
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| map_sql("reset_inflight begin", e))?;
            tx.execute(
                "UPDATE outbox SET state = ?1 WHERE state = ?2",
                rusqlite::params![OutboxState::Pending.as_i64(), OutboxState::Sending.as_i64()],
            )
            .map_err(|e| map_sql("reset_inflight outbox", e))?;
            tx.execute(
                "UPDATE messages SET status = ?1 WHERE status = ?2",
                rusqlite::params![
                    MessageStatus::Pending.as_i64(),
                    MessageStatus::Sending.as_i64()
                ],
            )
            .map_err(|e| map_sql("reset_inflight messages", e))?;
            tx.commit()
                .map_err(|e| map_sql("reset_inflight commit", e))?;
            Ok(())
        })
        .await
    }

    async fn apply_send_ack(&self, local_id: &str, server_id: u64) -> Result<(), StorageError> {
        let local_id = local_id.to_string();
        let server_id = to_sql_u64("apply_send_ack", server_id)?;
        let now = rchat_proto::now_ms() as i64;
        self.call(move |conn| {
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| map_sql("apply_send_ack begin", e))?;
            // Копия собственного сообщения могла прийти событием синхронизации
            // до подтверждения отправки: убираем дубль по server_id.
            tx.execute(
                "DELETE FROM messages WHERE server_id = ?1 AND local_id <> ?2",
                rusqlite::params![server_id, local_id],
            )
            .map_err(|e| map_sql("apply_send_ack dedup", e))?;
            tx.execute(
                "UPDATE messages SET server_id = ?1, status = ?2 WHERE local_id = ?3",
                rusqlite::params![server_id, MessageStatus::Sent.as_i64(), local_id],
            )
            .map_err(|e| map_sql("apply_send_ack message", e))?;
            tx.execute(
                "UPDATE chats
                 SET last_server_id = MAX(last_server_id, ?1),
                     updated_at_ms = MAX(updated_at_ms, ?2)
                 WHERE chat_id = (SELECT chat_id FROM messages WHERE local_id = ?3)",
                rusqlite::params![server_id, now, local_id],
            )
            .map_err(|e| map_sql("apply_send_ack chat", e))?;
            tx.execute(
                "DELETE FROM outbox WHERE local_id = ?1",
                rusqlite::params![local_id],
            )
            .map_err(|e| map_sql("apply_send_ack outbox", e))?;
            tx.commit()
                .map_err(|e| map_sql("apply_send_ack commit", e))?;
            Ok(())
        })
        .await
    }

    async fn apply_outbox_failure(
        &self,
        local_id: &str,
        state: OutboxState,
        error_code: Option<i32>,
        error: &str,
        next_attempt_at_ms: u64,
    ) -> Result<(), StorageError> {
        let local_id = local_id.to_string();
        let error = error.to_string();
        let next_attempt = to_sql_u64("apply_outbox_failure", next_attempt_at_ms)?;
        self.call(move |conn| {
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| map_sql("apply_outbox_failure begin", e))?;
            tx.execute(
                "UPDATE messages SET status = ?2 WHERE local_id = ?1",
                rusqlite::params![local_id, state.as_i64()],
            )
            .map_err(|e| map_sql("apply_outbox_failure message", e))?;
            tx.execute(
                "UPDATE outbox
                 SET state = ?2, attempts = attempts + 1, last_error_code = ?3,
                     last_error = ?4, next_attempt_at_ms = ?5
                 WHERE local_id = ?1",
                rusqlite::params![local_id, state.as_i64(), error_code, error, next_attempt],
            )
            .map_err(|e| map_sql("apply_outbox_failure outbox", e))?;
            tx.commit()
                .map_err(|e| map_sql("apply_outbox_failure commit", e))?;
            Ok(())
        })
        .await
    }

    // --- синхронизация ---

    async fn get_sync_cursor(&self) -> Result<u64, StorageError> {
        self.call(|conn| {
            let cursor: i64 = conn
                .query_row(
                    "SELECT COALESCE((SELECT cursor FROM sync_state WHERE id = 1), 0)",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| map_sql("get_sync_cursor", e))?;
            Ok(cursor.max(0) as u64)
        })
        .await
    }

    async fn set_sync_cursor(&self, seq: u64) -> Result<(), StorageError> {
        let seq = to_sql_u64("set_sync_cursor", seq)?;
        self.call(move |conn| {
            conn.execute(
                "INSERT INTO sync_state(id, cursor) VALUES(1, ?1)
                 ON CONFLICT(id) DO UPDATE SET cursor = excluded.cursor",
                rusqlite::params![seq],
            )
            .map_err(|e| map_sql("set_sync_cursor", e))?;
            Ok(())
        })
        .await
    }

    async fn apply_sync_batch(
        &self,
        batch: &[SyncApply],
        new_cursor: u64,
    ) -> Result<(), StorageError> {
        let batch = batch.to_vec();
        let new_cursor = to_sql_u64("apply_sync_batch", new_cursor)?;
        self.call(move |conn| {
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| map_sql("apply_sync_batch begin", e))?;
            for apply in &batch {
                apply_one(&tx, apply)?;
            }
            // Курсор только растёт: повторное применение старых пачек безопасно.
            tx.execute(
                "INSERT INTO sync_state(id, cursor) VALUES(1, ?1)
                 ON CONFLICT(id) DO UPDATE SET cursor = MAX(sync_state.cursor, excluded.cursor)",
                rusqlite::params![new_cursor],
            )
            .map_err(|e| map_sql("apply_sync_batch cursor", e))?;
            tx.commit()
                .map_err(|e| map_sql("apply_sync_batch commit", e))?;
            Ok(())
        })
        .await
    }

    // --- receipts ---

    async fn get_receipts(&self, chat_id: &str) -> Result<Vec<ReceiptRecord>, StorageError> {
        let chat_id = chat_id.to_string();
        self.call(move |conn| {
            let mut statement = conn
                .prepare(&format!(
                    "SELECT {RECEIPT_COLUMNS} FROM receipts WHERE chat_id = ?1 ORDER BY user_id"
                ))
                .map_err(|e| map_sql("get_receipts prepare", e))?;
            let rows = statement
                .query_map(rusqlite::params![chat_id], receipt_from_row)
                .map_err(|e| map_sql("get_receipts", e))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| map_sql("get_receipts row", e))
        })
        .await
    }

    // --- групповые E2E-сессии ---

    async fn group_key_shares(
        &self,
        chat_id: &str,
        session_id: &str,
    ) -> Result<Vec<String>, StorageError> {
        let chat_id = chat_id.to_string();
        let session_id = session_id.to_string();
        self.call(move |conn| {
            let mut statement = conn
                .prepare(
                    "SELECT device_id FROM group_key_shares
                     WHERE chat_id = ?1 AND session_id = ?2
                     ORDER BY device_id",
                )
                .map_err(|e| map_sql("group_key_shares prepare", e))?;
            let rows = statement
                .query_map(rusqlite::params![chat_id, session_id], |row| row.get(0))
                .map_err(|e| map_sql("group_key_shares", e))?;
            rows.collect::<Result<Vec<String>, _>>()
                .map_err(|e| map_sql("group_key_shares row", e))
        })
        .await
    }

    async fn add_group_key_shares(
        &self,
        chat_id: &str,
        session_id: &str,
        device_ids: &[String],
    ) -> Result<(), StorageError> {
        let chat_id = chat_id.to_string();
        let session_id = session_id.to_string();
        let device_ids = device_ids.to_vec();
        self.call(move |conn| {
            let now = to_sql_u64("group_key_shares now", rchat_proto::now_ms())?;
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| map_sql("group_key_shares begin", e))?;
            for device_id in &device_ids {
                tx.execute(
                    "INSERT INTO group_key_shares(chat_id, session_id, device_id, shared_at_ms)
                     VALUES(?1, ?2, ?3, ?4)
                     ON CONFLICT(chat_id, session_id, device_id) DO NOTHING",
                    rusqlite::params![chat_id, session_id, device_id, now],
                )
                .map_err(|e| map_sql("group_key_shares insert", e))?;
            }
            tx.commit()
                .map_err(|e| map_sql("group_key_shares commit", e))?;
            Ok(())
        })
        .await
    }

    async fn clear_group_key_shares(&self, chat_id: &str) -> Result<(), StorageError> {
        let chat_id = chat_id.to_string();
        self.call(move |conn| {
            conn.execute(
                "DELETE FROM group_key_shares WHERE chat_id = ?1",
                rusqlite::params![chat_id],
            )
            .map_err(|e| map_sql("clear_group_key_shares", e))?;
            Ok(())
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// Помощники, общие для нескольких методов
// ---------------------------------------------------------------------------

fn insert_message_sql(conn: &Connection, message: &MessageRecord) -> Result<(), StorageError> {
    let attachment_size = match &message.attachment {
        Some(attachment) => Some(to_sql_u64("attachment size", attachment.size)?),
        None => None,
    };
    conn.execute(
        &format!("INSERT INTO messages({MESSAGE_COLUMNS}) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)"),
        rusqlite::params![
            message.local_id,
            opt_to_sql_u64(message.server_id),
            message.chat_id,
            message.sender_user_id,
            message.sender_device_id,
            message.content_kind,
            message.body,
            opt_to_sql_u64(message.reply_to_server_id),
            message.status.map(|status| status.as_i64()),
            to_sql_u64("insert_message created", message.created_at_ms)?,
            opt_to_sql_u64(message.edited_at_ms),
            opt_to_sql_u64(message.deleted_at_ms),
            message.attachment.as_ref().map(|a| a.kind.as_str()),
            message.attachment.as_ref().map(|a| a.name.as_str()),
            message.attachment.as_ref().map(|a| a.mime.as_str()),
            attachment_size,
            message.attachment.as_ref().map(|a| a.data.as_slice()),
            message.attachment.as_ref().and_then(|a| a.blob_ref.as_deref()),
            message.attachment.as_ref().and_then(|a| a.key.as_deref()),
            message.attachment.as_ref().and_then(|a| a.nonce.as_deref()),
        ],
    )
    .map_err(|e| map_sql("insert_message", e))?;
    Ok(())
}

fn upsert_chat_sql(
    conn: &Connection,
    chat: &ChatRecord,
    members: &[MemberRecord],
) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO chats(chat_id, kind, e2e_mode, title, peer_user_id, created_at_ms,
                           updated_at_ms, last_server_id)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(chat_id) DO UPDATE SET
             kind = excluded.kind,
             e2e_mode = excluded.e2e_mode,
             title = excluded.title,
             peer_user_id = excluded.peer_user_id,
             created_at_ms = MIN(chats.created_at_ms, excluded.created_at_ms),
             updated_at_ms = MAX(chats.updated_at_ms, excluded.updated_at_ms),
             last_server_id = MAX(chats.last_server_id, excluded.last_server_id)",
        rusqlite::params![
            chat.chat_id,
            crate::helpers::chat_kind_to_sql(chat.kind),
            crate::helpers::e2e_mode_to_sql(chat.e2e_mode),
            chat.title,
            chat.peer_user_id,
            to_sql_u64("upsert_chat created", chat.created_at_ms)?,
            to_sql_u64("upsert_chat updated", chat.updated_at_ms)?,
            to_sql_u64("upsert_chat last_server_id", chat.last_server_id)?,
        ],
    )
    .map_err(|e| map_sql("upsert_chat", e))?;
    // Пустой список участников не затирает уже известный состав: сервер может
    // прислать чат без участников в части ответов.
    if !members.is_empty() {
        conn.execute(
            "DELETE FROM chat_members WHERE chat_id = ?1",
            rusqlite::params![chat.chat_id],
        )
        .map_err(|e| map_sql("upsert_chat members clear", e))?;
        for member in members {
            conn.execute(
                "INSERT INTO chat_members(chat_id, user_id, username, display_name, role, joined_at_ms)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(chat_id, user_id) DO UPDATE SET
                     username = excluded.username,
                     display_name = excluded.display_name,
                     role = excluded.role,
                     joined_at_ms = MIN(chat_members.joined_at_ms, excluded.joined_at_ms)",
                rusqlite::params![
                    chat.chat_id,
                    member.user_id,
                    member.username,
                    member.display_name,
                    crate::helpers::role_to_sql(member.role),
                    to_sql_u64("upsert_chat member joined", member.joined_at_ms)?,
                ],
            )
            .map_err(|e| map_sql("upsert_chat member", e))?;
            // Кеш профилей: из состава чата виден логин собеседника, а он
            // нужен для разрешения «логин → чат» без полного snapshot.
            if !member.username.is_empty() {
                conn.execute(
                    "INSERT INTO users(user_id, username, display_name) VALUES(?1, ?2, ?3)
                     ON CONFLICT(user_id) DO UPDATE SET
                         username = excluded.username,
                         display_name = excluded.display_name",
                    rusqlite::params![member.user_id, member.username, member.display_name],
                )
                .map_err(|e| map_sql("upsert_chat member user", e))?;
            }
        }
    }
    Ok(())
}

fn upsert_user_sql(conn: &Connection, user: &UserRecord) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO users(user_id, username, display_name) VALUES(?1, ?2, ?3)
         ON CONFLICT(user_id) DO UPDATE SET
             username = excluded.username,
             display_name = excluded.display_name",
        rusqlite::params![user.user_id, user.username, user.display_name],
    )
    .map_err(|e| map_sql("upsert_user", e))?;
    Ok(())
}

fn upsert_device_sql(conn: &Connection, device: &DeviceRecord) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO devices(device_id, user_id, name, platform, e2e_version, ed25519,
                             curve25519, revoked_at_ms)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(device_id) DO UPDATE SET
             user_id = excluded.user_id,
             name = excluded.name,
             platform = excluded.platform,
             e2e_version = excluded.e2e_version,
             -- Пустые ключи не должны затирать уже закешированные.
             ed25519 = CASE WHEN length(excluded.ed25519) = 0 AND length(devices.ed25519) > 0
                            THEN devices.ed25519 ELSE excluded.ed25519 END,
             curve25519 = CASE WHEN length(excluded.curve25519) = 0 AND length(devices.curve25519) > 0
                               THEN devices.curve25519 ELSE excluded.curve25519 END,
             revoked_at_ms = COALESCE(devices.revoked_at_ms, excluded.revoked_at_ms)",
        rusqlite::params![
            device.device_id,
            device.user_id,
            device.name,
            device.platform,
            device.e2e_version as i64,
            device.ed25519,
            device.curve25519,
            opt_to_sql_u64(device.revoked_at_ms),
        ],
    )
    .map_err(|e| map_sql("upsert_device", e))?;
    Ok(())
}

fn upsert_receipt_sql(conn: &Connection, receipt: &ReceiptRecord) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO receipts(chat_id, user_id, delivered_up_to, read_up_to, updated_at_ms)
         VALUES(?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(chat_id, user_id) DO UPDATE SET
             delivered_up_to = MAX(receipts.delivered_up_to, excluded.delivered_up_to),
             read_up_to = MAX(receipts.read_up_to, excluded.read_up_to),
             updated_at_ms = MAX(receipts.updated_at_ms, excluded.updated_at_ms)",
        rusqlite::params![
            receipt.chat_id,
            receipt.user_id,
            to_sql_u64("upsert_receipt delivered", receipt.delivered_up_to)?,
            to_sql_u64("upsert_receipt read", receipt.read_up_to)?,
            to_sql_u64("upsert_receipt updated", receipt.updated_at_ms)?,
        ],
    )
    .map_err(|e| map_sql("upsert_receipt", e))?;
    Ok(())
}

/// Применяет одну мутацию синхронизации. Все операции идемпотентны.
fn apply_one(tx: &Transaction<'_>, apply: &SyncApply) -> Result<(), StorageError> {
    match apply {
        SyncApply::UpsertChat { chat, members } => upsert_chat_sql(tx, chat, members),
        SyncApply::InsertMessage(message) => {
            let attachment_size = match &message.attachment {
                Some(attachment) => Some(to_sql_u64("attachment size", attachment.size)?),
                None => None,
            };
            tx.execute(
                &format!(
                    "INSERT INTO messages({MESSAGE_COLUMNS})
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
                     ON CONFLICT DO NOTHING"
                ),
                rusqlite::params![
                    message.local_id,
                    opt_to_sql_u64(message.server_id),
                    message.chat_id,
                    message.sender_user_id,
                    message.sender_device_id,
                    message.content_kind,
                    message.body,
                    opt_to_sql_u64(message.reply_to_server_id),
                    message.status.map(|status| status.as_i64()),
                    to_sql_u64("insert message created", message.created_at_ms)?,
                    opt_to_sql_u64(message.edited_at_ms),
                    opt_to_sql_u64(message.deleted_at_ms),
                    message.attachment.as_ref().map(|a| a.kind.as_str()),
                    message.attachment.as_ref().map(|a| a.name.as_str()),
                    message.attachment.as_ref().map(|a| a.mime.as_str()),
                    attachment_size,
                    message.attachment.as_ref().map(|a| a.data.as_slice()),
                    message.attachment.as_ref().and_then(|a| a.blob_ref.as_deref()),
                    message.attachment.as_ref().and_then(|a| a.key.as_deref()),
                    message.attachment.as_ref().and_then(|a| a.nonce.as_deref()),
                ],
            )
            .map_err(|e| map_sql("sync insert_message", e))?;
            // `last_server_id` чата — последнее известное локально сообщение
            // (нужно, например, для отметки прочтения «до последнего»).
            if let Some(server_id) = message.server_id {
                tx.execute(
                    "UPDATE chats SET last_server_id = MAX(last_server_id, ?2)
                     WHERE chat_id = ?1",
                    rusqlite::params![
                        message.chat_id,
                        to_sql_u64("insert message chat", server_id)?
                    ],
                )
                .map_err(|e| map_sql("sync insert_message chat", e))?;
            }
            Ok(())
        }
        SyncApply::EditMessage {
            server_id,
            content_kind,
            body,
            attachment,
            edited_at_ms,
        } => {
            let attachment_size = match &attachment {
                Some(item) => Some(to_sql_u64("sync edit attachment size", item.size)?),
                None => None,
            };
            tx.execute(
                "UPDATE messages
                 SET content_kind = ?2,
                     body = COALESCE(?3, body),
                     attachment_kind = COALESCE(?5, attachment_kind),
                     attachment_name = COALESCE(?6, attachment_name),
                     attachment_mime = COALESCE(?7, attachment_mime),
                     attachment_size = COALESCE(?8, attachment_size),
                     attachment_data = COALESCE(?9, attachment_data),
                     attachment_blob_ref = COALESCE(?10, attachment_blob_ref),
                     attachment_key = COALESCE(?11, attachment_key),
                     attachment_nonce = COALESCE(?12, attachment_nonce),
                     edited_at_ms = ?4
                 WHERE server_id = ?1",
                rusqlite::params![
                    to_sql_u64("sync edit server_id", *server_id)?,
                    content_kind,
                    body,
                    to_sql_u64("sync edit edited_at", *edited_at_ms)?,
                    attachment.as_ref().map(|a| a.kind.as_str()),
                    attachment.as_ref().map(|a| a.name.as_str()),
                    attachment.as_ref().map(|a| a.mime.as_str()),
                    attachment_size,
                    attachment.as_ref().map(|a| a.data.as_slice()),
                    attachment.as_ref().and_then(|a| a.blob_ref.as_deref()),
                    attachment.as_ref().and_then(|a| a.key.as_deref()),
                    attachment.as_ref().and_then(|a| a.nonce.as_deref()),
                ],
            )
            .map_err(|e| map_sql("sync edit_message", e))?;
            Ok(())
        }
        SyncApply::DeleteMessage {
            server_id,
            deleted_at_ms,
        } => {
            // Политика «удалить для всех»: содержимое стирается и в локальном
            // кеше, а не только помечается удалённым.
            tx.execute(
                "UPDATE messages
                 SET deleted_at_ms = ?2, body = NULL
                 WHERE server_id = ?1 AND (deleted_at_ms IS NULL OR deleted_at_ms > ?2)",
                rusqlite::params![
                    to_sql_u64("sync delete server_id", *server_id)?,
                    to_sql_u64("sync delete deleted_at", *deleted_at_ms)?,
                ],
            )
            .map_err(|e| map_sql("sync delete_message", e))?;
            Ok(())
        }
        SyncApply::UpsertReceipt(receipt) => upsert_receipt_sql(tx, receipt),
        SyncApply::UpsertUser(user) => upsert_user_sql(tx, user),
        SyncApply::UpsertDevice(device) => upsert_device_sql(tx, device),
        SyncApply::DeviceRevoked {
            device_id,
            revoked_at_ms,
        } => {
            tx.execute(
                "UPDATE devices SET revoked_at_ms = ?2
                 WHERE device_id = ?1 AND revoked_at_ms IS NULL",
                rusqlite::params![
                    device_id,
                    to_sql_u64("sync device revoked", *revoked_at_ms)?
                ],
            )
            .map_err(|e| map_sql("sync device_revoked", e))?;
            Ok(())
        }
        // Неизвестные события протокола осознанно пропускаются: курсор всё
        // равно продвигается (см. спецификацию).
        SyncApply::Skip { .. } => Ok(()),
    }
}

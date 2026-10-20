//! In-memory реализация клиентского хранилища.
//!
//! Повторяет документированную семантику SQLite-хранилища (монотонность
//! receipts и отзыва устройств, дедупликация по `server_id`, порядок
//! выборок, атомарность `apply_sync_batch`) и добавляет ручку инъекции сбоя
//! [`MockStorage::fail_next_batch`]: следующая пачка синхронизации вернёт
//! ошибку, не применив ни одной мутации и не продвинув курсор.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use async_trait::async_trait;
use rchat_core::error::{SecretStoreError, StorageError};
use rchat_core::traits::{CryptoStateStore, SecretStore, Storage};
use rchat_core::types::{
    ChatRecord, DeviceRecord, LocalAccount, MemberRecord, MessageRecord, MessageStatus,
    OutboxRecord, OutboxState, ReceiptRecord, SyncApply, UploadedBlob, UserRecord,
};

#[derive(Default, Clone)]
struct Inner {
    account: Option<LocalAccount>,
    users: BTreeMap<String, UserRecord>,
    devices: BTreeMap<String, DeviceRecord>,
    chats: BTreeMap<String, ChatRecord>,
    members: BTreeMap<String, Vec<MemberRecord>>,
    messages: BTreeMap<String, MessageRecord>,
    outbox: BTreeMap<String, OutboxRecord>,
    receipts: BTreeMap<(String, String), ReceiptRecord>,
    /// (chat_id, session_id) → выданные устройства.
    group_shares: BTreeMap<(String, String), BTreeSet<String>>,
    cursor: u64,
    crypto: BTreeMap<String, Vec<u8>>,
}

/// In-memory хранилище клиента (`Storage` + `CryptoStateStore`).
#[derive(Default)]
pub struct MockStorage {
    inner: Mutex<Inner>,
    fail_next_batch: AtomicBool,
}

impl MockStorage {
    pub fn new() -> Self {
        Self::default()
    }

    /// Следующий `apply_sync_batch` вернёт ошибку транзакции (мутации и
    /// курсор не изменятся). Сбрасывается после первого сбоя.
    pub fn fail_next_batch(&self) {
        self.fail_next_batch.store(true, Ordering::SeqCst);
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().expect("mock storage poisoned")
    }
}

fn transaction_error(message: &str) -> StorageError {
    StorageError::Transaction(message.to_string())
}

fn find_by_server(inner: &Inner, server_id: u64) -> Option<MessageRecord> {
    inner
        .messages
        .values()
        .find(|message| message.server_id == Some(server_id))
        .cloned()
}

fn apply_op(inner: &mut Inner, apply: &SyncApply) {
    match apply {
        SyncApply::UpsertChat { chat, members } => {
            if let Some(existing) = inner.chats.get(&chat.chat_id) {
                let mut merged = chat.clone();
                merged.last_server_id = existing.last_server_id.max(chat.last_server_id);
                merged.updated_at_ms = existing.updated_at_ms.max(chat.updated_at_ms);
                inner.chats.insert(chat.chat_id.clone(), merged);
            } else {
                inner.chats.insert(chat.chat_id.clone(), chat.clone());
            }
            inner.members.insert(chat.chat_id.clone(), members.clone());
        }
        SyncApply::InsertMessage(message) => {
            let duplicate_by_local = inner.messages.contains_key(&message.local_id);
            let duplicate_by_server = message
                .server_id
                .map(|server_id| find_by_server(inner, server_id).is_some())
                .unwrap_or(false);
            if duplicate_by_local || duplicate_by_server {
                return;
            }
            inner
                .messages
                .insert(message.local_id.clone(), message.clone());
            if let (Some(chat), Some(server_id)) =
                (inner.chats.get_mut(&message.chat_id), message.server_id)
            {
                chat.last_server_id = chat.last_server_id.max(server_id);
            }
        }
        SyncApply::EditMessage {
            server_id,
            content_kind,
            body,
            edited_at_ms,
            attachment: _,
        } => {
            if let Some(local_id) = inner
                .messages
                .values()
                .find(|message| message.server_id == Some(*server_id))
                .map(|message| message.local_id.clone())
            {
                if let Some(message) = inner.messages.get_mut(&local_id) {
                    message.content_kind = content_kind.clone();
                    if body.is_some() {
                        message.body = body.clone();
                    }
                    message.edited_at_ms = Some(
                        message
                            .edited_at_ms
                            .map(|old| old.max(*edited_at_ms))
                            .unwrap_or(*edited_at_ms),
                    );
                }
            }
        }
        SyncApply::DeleteMessage {
            server_id,
            deleted_at_ms,
        } => {
            if let Some(local_id) = inner
                .messages
                .values()
                .find(|message| message.server_id == Some(*server_id))
                .map(|message| message.local_id.clone())
            {
                if let Some(message) = inner.messages.get_mut(&local_id) {
                    // Как и SQLite-хранилище: сохраняется первая метка
                    // удаления, содержимое стирается.
                    let apply = message
                        .deleted_at_ms
                        .map(|old| old > *deleted_at_ms)
                        .unwrap_or(true);
                    if apply {
                        message.deleted_at_ms = Some(*deleted_at_ms);
                        message.body = None;
                    }
                }
            }
        }
        SyncApply::UpsertReceipt(receipt) => {
            let key = (receipt.chat_id.clone(), receipt.user_id.clone());
            let entry = inner.receipts.entry(key).or_insert_with(|| receipt.clone());
            entry.delivered_up_to = entry.delivered_up_to.max(receipt.delivered_up_to);
            entry.read_up_to = entry.read_up_to.max(receipt.read_up_to);
            entry.updated_at_ms = entry.updated_at_ms.max(receipt.updated_at_ms);
        }
        SyncApply::UpsertUser(user) => {
            inner.users.insert(user.user_id.clone(), user.clone());
        }
        SyncApply::UpsertDevice(device) => {
            let merged = match inner.devices.get(&device.device_id) {
                Some(existing) => {
                    let mut merged = device.clone();
                    if merged.ed25519.is_empty() {
                        merged.ed25519 = existing.ed25519.clone();
                    }
                    if merged.curve25519.is_empty() {
                        merged.curve25519 = existing.curve25519.clone();
                    }
                    if merged.revoked_at_ms.is_none() {
                        merged.revoked_at_ms = existing.revoked_at_ms;
                    }
                    merged.e2e_version = merged.e2e_version.max(existing.e2e_version);
                    merged
                }
                None => device.clone(),
            };
            inner.devices.insert(device.device_id.clone(), merged);
        }
        SyncApply::DeviceRevoked {
            device_id,
            revoked_at_ms,
        } => {
            if let Some(device) = inner.devices.get_mut(device_id) {
                device.revoked_at_ms = Some(
                    device
                        .revoked_at_ms
                        .map(|old| old.max(*revoked_at_ms))
                        .unwrap_or(*revoked_at_ms),
                );
            }
        }
        SyncApply::Skip { .. } => {}
    }
}

#[async_trait]
impl Storage for MockStorage {
    async fn migrate(&self) -> Result<(), StorageError> {
        Ok(())
    }

    async fn save_account(&self, account: &LocalAccount) -> Result<(), StorageError> {
        self.lock().account = Some(account.clone());
        Ok(())
    }

    async fn load_account(&self) -> Result<Option<LocalAccount>, StorageError> {
        Ok(self.lock().account.clone())
    }

    async fn clear_all(&self) -> Result<(), StorageError> {
        *self.lock() = Inner::default();
        Ok(())
    }

    async fn upsert_user(&self, user: &UserRecord) -> Result<(), StorageError> {
        self.lock().users.insert(user.user_id.clone(), user.clone());
        Ok(())
    }

    async fn get_user(&self, user_id: &str) -> Result<Option<UserRecord>, StorageError> {
        Ok(self.lock().users.get(user_id).cloned())
    }

    async fn list_users(&self) -> Result<Vec<UserRecord>, StorageError> {
        Ok(self.lock().users.values().cloned().collect())
    }

    async fn upsert_device(&self, device: &DeviceRecord) -> Result<(), StorageError> {
        let mut inner = self.lock();
        apply_op(&mut inner, &SyncApply::UpsertDevice(device.clone()));
        Ok(())
    }

    async fn get_device(&self, device_id: &str) -> Result<Option<DeviceRecord>, StorageError> {
        Ok(self.lock().devices.get(device_id).cloned())
    }

    async fn list_devices_of(&self, user_id: &str) -> Result<Vec<DeviceRecord>, StorageError> {
        let inner = self.lock();
        let mut devices: Vec<DeviceRecord> = inner
            .devices
            .values()
            .filter(|device| device.user_id == user_id)
            .cloned()
            .collect();
        devices.sort_by(|left, right| left.device_id.cmp(&right.device_id));
        Ok(devices)
    }

    async fn delete_device(&self, device_id: &str) -> Result<(), StorageError> {
        self.lock().devices.remove(device_id);
        Ok(())
    }

    async fn upsert_chat(
        &self,
        chat: &ChatRecord,
        members: &[MemberRecord],
    ) -> Result<(), StorageError> {
        let mut inner = self.lock();
        apply_op(
            &mut inner,
            &SyncApply::UpsertChat {
                chat: chat.clone(),
                members: members.to_vec(),
            },
        );
        Ok(())
    }

    async fn replace_chats(
        &self,
        chats: &[(ChatRecord, Vec<MemberRecord>)],
    ) -> Result<(), StorageError> {
        let mut inner = self.lock();
        inner.chats.clear();
        inner.members.clear();
        for (chat, members) in chats {
            inner.chats.insert(chat.chat_id.clone(), chat.clone());
            inner.members.insert(chat.chat_id.clone(), members.clone());
        }
        Ok(())
    }

    async fn list_chats(&self) -> Result<Vec<ChatRecord>, StorageError> {
        let inner = self.lock();
        let mut chats: Vec<ChatRecord> = inner.chats.values().cloned().collect();
        chats.sort_by(|left, right| {
            right
                .updated_at_ms
                .cmp(&left.updated_at_ms)
                .then_with(|| left.chat_id.cmp(&right.chat_id))
        });
        Ok(chats)
    }

    async fn get_chat(&self, chat_id: &str) -> Result<Option<ChatRecord>, StorageError> {
        Ok(self.lock().chats.get(chat_id).cloned())
    }

    async fn chat_members(&self, chat_id: &str) -> Result<Vec<MemberRecord>, StorageError> {
        Ok(self
            .lock()
            .members
            .get(chat_id)
            .cloned()
            .unwrap_or_default())
    }

    // --- файлы: локальный кэш и дедупликация (в моке — без эффекта) ---
    async fn cache_attachment_data(
        &self,
        _server_id: u64,
        _data: &[u8],
        _max_total_bytes: u64,
    ) -> Result<bool, StorageError> {
        Ok(false)
    }

    async fn find_uploaded_blob(
        &self,
        _hash: &[u8; 32],
    ) -> Result<Option<UploadedBlob>, StorageError> {
        Ok(None)
    }

    async fn save_uploaded_blob(
        &self,
        _hash: &[u8; 32],
        _blob_ref: &str,
        _key: &[u8],
        _nonce: &[u8],
        _size: u64,
    ) -> Result<(), StorageError> {
        Ok(())
    }

    async fn get_message(&self, local_id: &str) -> Result<Option<MessageRecord>, StorageError> {
        Ok(self.lock().messages.get(local_id).cloned())
    }

    async fn get_message_by_server(
        &self,
        server_id: u64,
    ) -> Result<Option<MessageRecord>, StorageError> {
        Ok(find_by_server(&self.lock(), server_id))
    }

    async fn last_messages(
        &self,
        chat_ids: &[String],
    ) -> Result<Vec<Option<MessageRecord>>, StorageError> {
        let inner = self.lock();
        let mut result: Vec<Option<MessageRecord>> = vec![None; chat_ids.len()];
        for (index, chat_id) in chat_ids.iter().enumerate() {
            let mut messages: Vec<MessageRecord> = inner
                .messages
                .values()
                .filter(|message| &message.chat_id == chat_id)
                .cloned()
                .collect();
            // Локальные сообщения без server_id считаются самыми новыми.
            messages.sort_by(|left, right| {
                right
                    .server_id
                    .cmp(&left.server_id)
                    .then(right.created_at_ms.cmp(&left.created_at_ms))
            });
            result[index] = messages.into_iter().next();
        }
        Ok(result)
    }

    async fn list_messages(
        &self,
        chat_id: &str,
        before_server_id: Option<u64>,
        limit: u32,
    ) -> Result<Vec<MessageRecord>, StorageError> {
        let inner = self.lock();
        let mut messages: Vec<MessageRecord> = inner
            .messages
            .values()
            .filter(|message| message.chat_id == chat_id)
            .filter(|message| match before_server_id {
                Some(before) => message.server_id.is_some_and(|id| id < before),
                None => true,
            })
            .cloned()
            .collect();
        // Локальные сообщения без server_id считаются самыми новыми.
        messages.sort_by(|left, right| {
            right
                .server_id
                .unwrap_or(u64::MAX)
                .cmp(&left.server_id.unwrap_or(u64::MAX))
                .then_with(|| right.created_at_ms.cmp(&left.created_at_ms))
        });
        messages.truncate(limit as usize);
        Ok(messages)
    }

    async fn insert_outgoing(
        &self,
        message: &MessageRecord,
        outbox: &OutboxRecord,
    ) -> Result<(), StorageError> {
        let mut inner = self.lock();
        inner
            .messages
            .insert(message.local_id.clone(), message.clone());
        inner.outbox.insert(outbox.local_id.clone(), outbox.clone());
        Ok(())
    }

    async fn get_outbox(&self, local_id: &str) -> Result<Option<OutboxRecord>, StorageError> {
        Ok(self.lock().outbox.get(local_id).cloned())
    }

    async fn update_outbox(&self, outbox: &OutboxRecord) -> Result<(), StorageError> {
        self.lock()
            .outbox
            .insert(outbox.local_id.clone(), outbox.clone());
        Ok(())
    }

    async fn delete_outbox(&self, local_id: &str) -> Result<(), StorageError> {
        self.lock().outbox.remove(local_id);
        Ok(())
    }

    async fn due_outbox(&self, now_ms: u64, limit: u32) -> Result<Vec<OutboxRecord>, StorageError> {
        let inner = self.lock();
        let mut due: Vec<OutboxRecord> = inner
            .outbox
            .values()
            .filter(|record| record.state != OutboxState::Sent)
            .filter(|record| record.next_attempt_at_ms <= now_ms)
            .cloned()
            .collect();
        due.sort_by(|left, right| {
            left.created_at_ms
                .cmp(&right.created_at_ms)
                .then_with(|| left.local_id.cmp(&right.local_id))
        });
        due.truncate(limit as usize);
        Ok(due)
    }

    async fn reset_inflight_outbox(&self) -> Result<(), StorageError> {
        let mut inner = self.lock();
        for record in inner.outbox.values_mut() {
            if record.state == OutboxState::Sending {
                record.state = OutboxState::Pending;
                record.next_attempt_at_ms = 0;
            }
        }
        Ok(())
    }

    async fn apply_send_ack(&self, local_id: &str, server_id: u64) -> Result<(), StorageError> {
        let mut inner = self.lock();
        // «Эхо»-копия, пришедшая событием раньше подтверждения, удаляется.
        let echo: Vec<String> = inner
            .messages
            .iter()
            .filter(|(key, message)| {
                key.as_str() != local_id && message.server_id == Some(server_id)
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in echo {
            inner.messages.remove(&key);
        }
        let chat_id = if let Some(message) = inner.messages.get_mut(local_id) {
            message.server_id = Some(server_id);
            message.status = Some(MessageStatus::Sent);
            Some(message.chat_id.clone())
        } else {
            None
        };
        inner.outbox.remove(local_id);
        if let Some(chat_id) = chat_id {
            if let Some(chat) = inner.chats.get_mut(&chat_id) {
                chat.last_server_id = chat.last_server_id.max(server_id);
            }
        }
        Ok(())
    }

    async fn requeue_outbox(&self, local_id: &str) -> Result<bool, StorageError> {
        let mut inner = self.lock();
        let Some(record) = inner.outbox.get_mut(local_id) else {
            return Ok(false);
        };
        if record.state != OutboxState::Failed {
            return Ok(false);
        }
        record.state = OutboxState::Pending;
        record.attempts = 0;
        record.last_error_code = None;
        record.last_error = None;
        record.next_attempt_at_ms = 0;
        if let Some(message) = inner.messages.get_mut(local_id) {
            message.status = Some(MessageStatus::Sending);
        }
        Ok(true)
    }

    async fn apply_outbox_failure(
        &self,
        local_id: &str,
        state: OutboxState,
        error_code: Option<i32>,
        error: &str,
        next_attempt_at_ms: u64,
    ) -> Result<(), StorageError> {
        let mut inner = self.lock();
        if let Some(message) = inner.messages.get_mut(local_id) {
            message.status = Some(match state {
                OutboxState::Pending => MessageStatus::Pending,
                OutboxState::Sending => MessageStatus::Sending,
                OutboxState::Sent => MessageStatus::Sent,
                OutboxState::Failed => MessageStatus::Failed,
            });
        }
        if let Some(record) = inner.outbox.get_mut(local_id) {
            record.state = state;
            record.attempts += 1;
            record.last_error_code = error_code;
            record.last_error = Some(error.to_string());
            record.next_attempt_at_ms = next_attempt_at_ms;
        }
        Ok(())
    }

    async fn get_sync_cursor(&self) -> Result<u64, StorageError> {
        Ok(self.lock().cursor)
    }

    async fn set_sync_cursor(&self, seq: u64) -> Result<(), StorageError> {
        let mut inner = self.lock();
        inner.cursor = inner.cursor.max(seq);
        Ok(())
    }

    async fn apply_sync_batch(
        &self,
        batch: &[SyncApply],
        new_cursor: u64,
    ) -> Result<(), StorageError> {
        if self.fail_next_batch.swap(false, Ordering::SeqCst) {
            return Err(transaction_error("injected sync batch failure"));
        }
        let mut inner = self.lock();
        let mut next = inner.clone();
        for apply in batch {
            apply_op(&mut next, apply);
        }
        next.cursor = next.cursor.max(new_cursor);
        *inner = next;
        Ok(())
    }

    async fn get_receipts(&self, chat_id: &str) -> Result<Vec<ReceiptRecord>, StorageError> {
        let inner = self.lock();
        let mut receipts: Vec<ReceiptRecord> = inner
            .receipts
            .values()
            .filter(|receipt| receipt.chat_id == chat_id)
            .cloned()
            .collect();
        receipts.sort_by(|left, right| left.user_id.cmp(&right.user_id));
        Ok(receipts)
    }

    async fn group_key_shares(
        &self,
        chat_id: &str,
        session_id: &str,
    ) -> Result<Vec<String>, StorageError> {
        Ok(self
            .lock()
            .group_shares
            .get(&(chat_id.to_string(), session_id.to_string()))
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default())
    }

    async fn add_group_key_shares(
        &self,
        chat_id: &str,
        session_id: &str,
        device_ids: &[String],
    ) -> Result<(), StorageError> {
        let mut inner = self.lock();
        let entry = inner
            .group_shares
            .entry((chat_id.to_string(), session_id.to_string()))
            .or_default();
        for device_id in device_ids {
            entry.insert(device_id.clone());
        }
        Ok(())
    }

    async fn clear_group_key_shares(&self, chat_id: &str) -> Result<(), StorageError> {
        self.lock()
            .group_shares
            .retain(|(chat, _), _| chat != chat_id);
        Ok(())
    }
}

#[async_trait]
impl CryptoStateStore for MockStorage {
    async fn crypto_load(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        Ok(self.lock().crypto.get(key).cloned())
    }

    async fn crypto_save(&self, key: &str, value: &[u8]) -> Result<(), StorageError> {
        self.lock().crypto.insert(key.to_string(), value.to_vec());
        Ok(())
    }

    async fn crypto_delete(&self, key: &str) -> Result<(), StorageError> {
        self.lock().crypto.remove(key);
        Ok(())
    }

    async fn crypto_delete_prefix(&self, prefix: &str) -> Result<u64, StorageError> {
        let mut inner = self.lock();
        let keys: Vec<String> = inner
            .crypto
            .keys()
            .filter(|key| key.starts_with(prefix))
            .cloned()
            .collect();
        for key in &keys {
            inner.crypto.remove(key);
        }
        Ok(keys.len() as u64)
    }
}

/// In-memory хранилище секретов (для тестов, без шифрования файла).
#[derive(Default)]
pub struct MockSecretStore {
    values: Mutex<BTreeMap<String, Vec<u8>>>,
}

impl MockSecretStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for MockSecretStore {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        Ok(self
            .values
            .lock()
            .expect("mock secrets poisoned")
            .get(key)
            .cloned())
    }

    fn set(&self, key: &str, value: &[u8]) -> Result<(), SecretStoreError> {
        self.values
            .lock()
            .expect("mock secrets poisoned")
            .insert(key.to_string(), value.to_vec());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
        self.values
            .lock()
            .expect("mock secrets poisoned")
            .remove(key);
        Ok(())
    }
}

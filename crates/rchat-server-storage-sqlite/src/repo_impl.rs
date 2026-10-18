//! Единственная реализация трейта [`Repo`] для [`SqliteRepo`].
//!
//! SQL-код держится здесь; вспомогательные функции — в [`crate::helpers`].

use async_trait::async_trait;
use rchat_server::model::*;
use rchat_server::repo::{Repo, RepoError};
use rusqlite::{params, OptionalExtension};

use crate::convert::*;
use crate::helpers::*;
use crate::{map_sql, SqliteRepo};

#[async_trait]
impl Repo for SqliteRepo {
    async fn create_user_with_device(
        &self,
        user: &NewUser,
        device: &NewDevice,
        session: &NewSession,
    ) -> Result<(UserRecord, SessionRecord), RepoError> {
        let user = user.clone();
        let device = device.clone();
        let session = session.clone();
        self.call(move |conn| {
            let tx = conn
                .transaction()
                .map_err(|e| RepoError::internal(format!("begin register: {e}")))?;
            let user_id = random_id("user id")?;
            tx.execute(
                "INSERT INTO users(user_id, username, display_name, password_hash, created_at_ms, updated_at_ms, disabled)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?5, 0)",
                params![
                    user_id,
                    user.username,
                    user.display_name,
                    user.password_hash,
                    user.created_at_ms as i64
                ],
            )
            .map_err(|e| map_sql("insert user", e))?;
            tx.execute(
                "INSERT INTO devices(device_id, user_id, name, platform, e2e_version, ed25519, curve25519, created_at_ms, last_seen_at_ms, revoked_at_ms)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, 0)",
                params![
                    device.device_id,
                    user_id,
                    device.name,
                    device.platform,
                    device.e2e_version as i64,
                    device.ed25519,
                    device.curve25519,
                    user.created_at_ms as i64
                ],
            )
            .map_err(|e| map_sql("insert device", e))?;
            insert_prekeys(
                &tx,
                &device.device_id,
                &device.one_time_keys,
                device.fallback_key.as_ref(),
            )?;
            let session_id = random_id("session id")?;
            tx.execute(
                "INSERT INTO sessions(session_id, token_hash, user_id, device_id, created_at_ms, expires_at_ms, last_used_at_ms, revoked_at_ms)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?5, 0)",
                params![
                    session_id,
                    session.token_hash,
                    user_id,
                    session.device_id,
                    session.created_at_ms as i64,
                    session.expires_at_ms as i64
                ],
            )
            .map_err(|e| map_sql("insert session", e))?;
            tx.commit()
                .map_err(|e| RepoError::internal(format!("commit register: {e}")))?;
            let user_record = UserRecord {
                user_id,
                username: user.username,
                display_name: user.display_name,
                password_hash: user.password_hash,
                created_at_ms: user.created_at_ms,
                updated_at_ms: user.created_at_ms,
                disabled: false,
            };
            let session_record = SessionRecord {
                session_id,
                token_hash: session.token_hash,
                user_id: user_record.user_id.clone(),
                device_id: session.device_id,
                created_at_ms: session.created_at_ms,
                expires_at_ms: session.expires_at_ms,
                last_used_at_ms: session.created_at_ms,
                revoked_at_ms: 0,
            };
            Ok((user_record, session_record))
        })
        .await
    }

    async fn find_user_by_username(&self, username: &str) -> Result<Option<UserRecord>, RepoError> {
        let username = username.to_string();
        self.call(move |conn| {
            let sql = format!("SELECT {USER_COLUMNS} FROM users WHERE username = ?1");
            conn.query_row(&sql, params![username], user_from_row)
                .optional()
                .map_err(|e| RepoError::internal(format!("find_user_by_username: {e}")))
        })
        .await
    }

    async fn find_user_by_id(&self, user_id: &str) -> Result<Option<UserRecord>, RepoError> {
        let user_id = user_id.to_string();
        self.call(move |conn| find_user(conn, &user_id)).await
    }

    async fn find_users_by_ids(&self, user_ids: &[String]) -> Result<Vec<UserRecord>, RepoError> {
        if user_ids.is_empty() {
            return Ok(Vec::new());
        }
        let user_ids = user_ids.to_vec();
        self.call(move |conn| {
            let placeholders: Vec<String> = (1..=user_ids.len()).map(|i| format!("?{i}")).collect();
            let sql = format!(
                "SELECT {USER_COLUMNS} FROM users WHERE user_id IN ({}) ORDER BY username",
                placeholders.join(", ")
            );
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| RepoError::internal(format!("prepare: {e}")))?;
            let params: Vec<&dyn rusqlite::ToSql> = user_ids
                .iter()
                .map(|id| id as &dyn rusqlite::ToSql)
                .collect();
            let rows = stmt
                .query_map(params.as_slice(), user_from_row)
                .map_err(|e| RepoError::internal(format!("query: {e}")))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| RepoError::internal(format!("rows: {e}")))
        })
        .await
    }

    async fn find_device(
        &self,
        user_id: &str,
        device_id: &str,
    ) -> Result<Option<DeviceRecord>, RepoError> {
        let user_id = user_id.to_string();
        let device_id = device_id.to_string();
        self.call(move |conn| find_device(conn, &user_id, &device_id))
            .await
    }

    async fn find_device_by_id(&self, device_id: &str) -> Result<Option<DeviceRecord>, RepoError> {
        let device_id = device_id.to_string();
        self.call(move |conn| find_device_by_id(conn, &device_id))
            .await
    }

    async fn create_device(
        &self,
        user_id: &str,
        device: &NewDevice,
    ) -> Result<DeviceRecord, RepoError> {
        let user_id = user_id.to_string();
        let device = device.clone();
        self.call(move |conn| {
            let now = rchat_proto::now_ms();
            let tx = conn
                .transaction()
                .map_err(|e| RepoError::internal(format!("begin create_device: {e}")))?;
            tx.execute(
                "INSERT INTO devices(device_id, user_id, name, platform, e2e_version, ed25519, curve25519, created_at_ms, last_seen_at_ms, revoked_at_ms)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, 0)",
                params![
                    device.device_id,
                    user_id,
                    device.name,
                    device.platform,
                    device.e2e_version as i64,
                    device.ed25519,
                    device.curve25519,
                    now as i64
                ],
            )
            .map_err(|e| map_sql("insert device", e))?;
            insert_prekeys(
                &tx,
                &device.device_id,
                &device.one_time_keys,
                device.fallback_key.as_ref(),
            )?;
            tx.commit()
                .map_err(|e| RepoError::internal(format!("commit create_device: {e}")))?;
            Ok(DeviceRecord {
                device_id: device.device_id,
                user_id,
                name: device.name,
                platform: device.platform,
                e2e_version: device.e2e_version,
                ed25519: device.ed25519,
                curve25519: device.curve25519,
                created_at_ms: now,
                last_seen_at_ms: now,
                revoked_at_ms: 0,
            })
        })
        .await
    }

    async fn update_device_keys(
        &self,
        user_id: &str,
        device_id: &str,
        e2e_version: u32,
        ed25519: &[u8],
        curve25519: &[u8],
    ) -> Result<(), RepoError> {
        let user_id = user_id.to_string();
        let device_id = device_id.to_string();
        let ed25519 = ed25519.to_vec();
        let curve25519 = curve25519.to_vec();
        self.call(move |conn| {
            let changed = conn
                .execute(
                    "UPDATE devices SET e2e_version = ?3, ed25519 = ?4, curve25519 = ?5
                     WHERE user_id = ?1 AND device_id = ?2",
                    params![user_id, device_id, e2e_version as i64, ed25519, curve25519],
                )
                .map_err(|e| map_sql("update device keys", e))?;
            if changed == 0 {
                return Err(RepoError::NotFound(format!("device {device_id}")));
            }
            Ok(())
        })
        .await
    }

    async fn touch_device(
        &self,
        user_id: &str,
        device_id: &str,
        now_ms: u64,
    ) -> Result<(), RepoError> {
        let user_id = user_id.to_string();
        let device_id = device_id.to_string();
        self.call(move |conn| {
            conn.execute(
                "UPDATE devices SET last_seen_at_ms = ?3 WHERE user_id = ?1 AND device_id = ?2",
                params![user_id, device_id, now_ms as i64],
            )
            .map_err(|e| RepoError::internal(format!("touch device: {e}")))?;
            Ok(())
        })
        .await
    }

    async fn revoke_device(
        &self,
        user_id: &str,
        device_id: &str,
        now_ms: u64,
    ) -> Result<DeviceRecord, RepoError> {
        let user_id = user_id.to_string();
        let device_id = device_id.to_string();
        self.call(move |conn| {
            let tx = conn
                .transaction()
                .map_err(|e| RepoError::internal(format!("begin revoke: {e}")))?;
            tx.execute(
                "UPDATE devices SET revoked_at_ms = ?3 WHERE user_id = ?1 AND device_id = ?2 AND revoked_at_ms = 0",
                params![user_id, device_id, now_ms as i64],
            )
            .map_err(|e| map_sql("revoke device", e))?;
            tx.execute(
                "UPDATE sessions SET revoked_at_ms = ?2 WHERE device_id = ?1 AND revoked_at_ms = 0",
                params![device_id, now_ms as i64],
            )
            .map_err(|e| map_sql("revoke sessions", e))?;
            tx.commit()
                .map_err(|e| RepoError::internal(format!("commit revoke: {e}")))?;
            find_device(conn, &user_id, &device_id)?
                .ok_or_else(|| RepoError::NotFound(format!("device {device_id}")))
        })
        .await
    }

    async fn list_devices(
        &self,
        user_id: &str,
        include_revoked: bool,
    ) -> Result<Vec<DeviceRecord>, RepoError> {
        let user_id = user_id.to_string();
        self.call(move |conn| list_devices(conn, &user_id, include_revoked))
            .await
    }

    async fn devices_for_users(
        &self,
        user_ids: &[String],
        include_revoked: bool,
    ) -> Result<Vec<DeviceRecord>, RepoError> {
        let user_ids = user_ids.to_vec();
        self.call(move |conn| devices_for_users(conn, &user_ids, include_revoked))
            .await
    }

    async fn store_prekeys(
        &self,
        user_id: &str,
        device_id: &str,
        keys: &[PreKeyRecord],
        fallback: Option<&PreKeyRecord>,
    ) -> Result<u32, RepoError> {
        let user_id = user_id.to_string();
        let device_id = device_id.to_string();
        let keys = keys.to_vec();
        let fallback = fallback.cloned();
        self.call(move |conn| {
            if find_device(conn, &user_id, &device_id)?.is_none() {
                return Err(RepoError::NotFound(format!("device {device_id}")));
            }
            let tx = conn
                .transaction()
                .map_err(|e| RepoError::internal(format!("begin prekeys: {e}")))?;
            let inserted = insert_prekeys(&tx, &device_id, &keys, fallback.as_ref())?;
            tx.commit()
                .map_err(|e| RepoError::internal(format!("commit prekeys: {e}")))?;
            Ok(inserted)
        })
        .await
    }

    async fn take_bundle(
        &self,
        user_id: &str,
        device_id: &str,
    ) -> Result<Option<BundleRecord>, RepoError> {
        let user_id = user_id.to_string();
        let device_id = device_id.to_string();
        self.call(move |conn| {
            let Some(device) = find_device(conn, &user_id, &device_id)? else {
                return Ok(None);
            };
            if device.is_revoked() {
                return Ok(None);
            }
            let tx = conn
                .transaction()
                .map_err(|e| RepoError::internal(format!("begin bundle: {e}")))?;
            let one_time = {
                let mut stmt = tx
                    .prepare(
                        "SELECT key_id, public_key, fallback, created_at_ms FROM prekeys
                         WHERE device_id = ?1 AND fallback = 0 AND consumed_at_ms = 0
                         ORDER BY created_at_ms, key_id LIMIT 1",
                    )
                    .map_err(|e| RepoError::internal(format!("prepare prekey: {e}")))?;
                let mut rows = stmt
                    .query_map(params![device_id], prekey_from_row)
                    .map_err(|e| RepoError::internal(format!("query prekey: {e}")))?;
                match rows.next() {
                    Some(row) => Some(row.map_err(|e| RepoError::internal(format!("row: {e}")))?),
                    None => None,
                }
            };
            let (one_time_key, fallback) = match one_time {
                Some(key) => {
                    tx.execute(
                        "UPDATE prekeys SET consumed_at_ms = ?2 WHERE device_id = ?1 AND key_id = ?3",
                        params![device_id, rchat_proto::now_ms() as i64, key.key_id as i64],
                    )
                    .map_err(|e| RepoError::internal(format!("consume prekey: {e}")))?;
                    (Some(key), false)
                }
                None => {
                    let mut stmt = tx
                        .prepare(
                            "SELECT key_id, public_key, fallback, created_at_ms FROM prekeys
                             WHERE device_id = ?1 AND fallback = 1
                             ORDER BY created_at_ms DESC LIMIT 1",
                        )
                        .map_err(|e| RepoError::internal(format!("prepare fallback: {e}")))?;
                    let mut rows = stmt
                        .query_map(params![device_id], prekey_from_row)
                        .map_err(|e| RepoError::internal(format!("query fallback: {e}")))?;
                    let key = match rows.next() {
                        Some(row) => Some(row.map_err(|e| RepoError::internal(format!("row: {e}")))?),
                        None => None,
                    };
                    let used_fallback = key.is_some();
                    (key, used_fallback)
                }
            };
            tx.commit()
                .map_err(|e| RepoError::internal(format!("commit bundle: {e}")))?;
            Ok(Some(BundleRecord {
                device,
                one_time_key,
                fallback,
            }))
        })
        .await
    }

    async fn create_session(
        &self,
        user_id: &str,
        session: &NewSession,
    ) -> Result<SessionRecord, RepoError> {
        let user_id = user_id.to_string();
        let session = session.clone();
        self.call(move |conn| {
            let session_id = random_id("session id")?;
            conn.execute(
                "INSERT INTO sessions(session_id, token_hash, user_id, device_id, created_at_ms, expires_at_ms, last_used_at_ms, revoked_at_ms)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?5, 0)",
                params![
                    session_id,
                    session.token_hash,
                    user_id,
                    session.device_id,
                    session.created_at_ms as i64,
                    session.expires_at_ms as i64
                ],
            )
            .map_err(|e| map_sql("insert session", e))?;
            Ok(SessionRecord {
                session_id,
                token_hash: session.token_hash,
                user_id,
                device_id: session.device_id,
                created_at_ms: session.created_at_ms,
                expires_at_ms: session.expires_at_ms,
                last_used_at_ms: session.created_at_ms,
                revoked_at_ms: 0,
            })
        })
        .await
    }

    async fn find_session_by_token_hash(
        &self,
        token_hash: &[u8],
    ) -> Result<Option<SessionRecord>, RepoError> {
        let token_hash = token_hash.to_vec();
        self.call(move |conn| {
            let sql = format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE token_hash = ?1");
            conn.query_row(&sql, params![token_hash], session_from_row)
                .optional()
                .map_err(|e| RepoError::internal(format!("find_session: {e}")))
        })
        .await
    }

    async fn touch_session(&self, session_id: &str, now_ms: u64) -> Result<(), RepoError> {
        let session_id = session_id.to_string();
        self.call(move |conn| {
            conn.execute(
                "UPDATE sessions SET last_used_at_ms = ?2 WHERE session_id = ?1",
                params![session_id, now_ms as i64],
            )
            .map_err(|e| RepoError::internal(format!("touch session: {e}")))?;
            Ok(())
        })
        .await
    }

    async fn revoke_session(&self, session_id: &str, now_ms: u64) -> Result<(), RepoError> {
        let session_id = session_id.to_string();
        self.call(move |conn| {
            conn.execute(
                "UPDATE sessions SET revoked_at_ms = ?2 WHERE session_id = ?1 AND revoked_at_ms = 0",
                params![session_id, now_ms as i64],
            )
            .map_err(|e| RepoError::internal(format!("revoke session: {e}")))?;
            Ok(())
        })
        .await
    }
    // --- чаты ---

    async fn find_direct_chat(
        &self,
        user_a: &str,
        user_b: &str,
    ) -> Result<Option<(ChatRecord, Vec<MemberRecord>)>, RepoError> {
        let key = pair_key(user_a, user_b);
        self.call(move |conn| {
            let sql = format!("SELECT {CHAT_COLUMNS} FROM chats WHERE pair_key = ?1");
            let chat: Option<ChatRecord> = conn
                .query_row(&sql, params![key], chat_from_row)
                .optional()
                .map_err(|e| RepoError::internal(format!("find_direct_chat: {e}")))?;
            match chat {
                Some(chat) => {
                    let chat_id = chat.chat_id.clone();
                    let members = members_of(conn, &chat_id)?;
                    Ok(Some((chat, members)))
                }
                None => Ok(None),
            }
        })
        .await
    }

    async fn create_chat(
        &self,
        chat: &ChatRecord,
        members: &[MemberRecord],
        events: &[NewSyncEvent],
    ) -> Result<Vec<(String, u64)>, RepoError> {
        let chat = chat.clone();
        let members = members.to_vec();
        let events = events.to_vec();
        self.call(move |conn| {
            let tx = conn
                .transaction()
                .map_err(|e| RepoError::internal(format!("begin create_chat: {e}")))?;
            let pair = if chat.kind == rchat_proto::proto::ChatKind::Direct as i32 {
                let ids: Vec<&str> = members.iter().map(|m| m.user_id.as_str()).collect();
                match ids.as_slice() {
                    [a, b] => Some(pair_key(a, b)),
                    _ => {
                        return Err(RepoError::internal(
                            "direct chat must have exactly two members",
                        ));
                    }
                }
            } else {
                None
            };
            tx.execute(
                "INSERT INTO chats(chat_id, kind, e2e_mode, name, pair_key, created_at_ms, updated_at_ms, last_server_id)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    chat.chat_id,
                    chat.kind as i64,
                    chat.e2e_mode as i64,
                    chat.name,
                    pair,
                    chat.created_at_ms as i64,
                    chat.updated_at_ms as i64,
                    chat.last_server_id as i64
                ],
            )
            .map_err(|e| map_sql("insert chat", e))?;
            for member in &members {
                tx.execute(
                    "INSERT INTO chat_members(chat_id, user_id, joined_at_ms, role) VALUES(?1, ?2, ?3, ?4)",
                    params![
                        member.chat_id,
                        member.user_id,
                        member.joined_at_ms as i64,
                        member.role as i64
                    ],
                )
                .map_err(|e| map_sql("insert member", e))?;
            }
            let seqs = insert_events(&tx, &events)?;
            tx.commit()
                .map_err(|e| RepoError::internal(format!("commit create_chat: {e}")))?;
            Ok(seqs)
        })
        .await
    }

    async fn get_chat(
        &self,
        chat_id: &str,
    ) -> Result<Option<(ChatRecord, Vec<MemberRecord>)>, RepoError> {
        let chat_id = chat_id.to_string();
        self.call(move |conn| get_chat(conn, &chat_id)).await
    }

    async fn list_chats_for_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<(ChatRecord, Vec<MemberRecord>)>, RepoError> {
        let user_id = user_id.to_string();
        self.call(move |conn| {
            let sql = "SELECT c.chat_id, c.kind, c.e2e_mode, c.name, c.created_at_ms, c.updated_at_ms, c.last_server_id
                 FROM chats c JOIN chat_members m ON m.chat_id = c.chat_id
                 WHERE m.user_id = ?1 ORDER BY c.updated_at_ms DESC, c.chat_id";
            let mut stmt = conn
                .prepare(sql)
                .map_err(|e| RepoError::internal(format!("prepare list_chats: {e}")))?;
            let chats: Vec<ChatRecord> = stmt
                .query_map(params![user_id], chat_from_row)
                .map_err(|e| RepoError::internal(format!("query list_chats: {e}")))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| RepoError::internal(format!("rows list_chats: {e}")))?;
            let mut result = Vec::with_capacity(chats.len());
            for chat in chats {
                let members = members_of(conn, &chat.chat_id)?;
                result.push((chat, members));
            }
            Ok(result)
        })
        .await
    }

    async fn has_shared_chat(&self, user_a: &str, user_b: &str) -> Result<bool, RepoError> {
        let user_a = user_a.to_string();
        let user_b = user_b.to_string();
        self.call(move |conn| {
            let found: Option<i64> = conn
                .query_row(
                    "SELECT 1 FROM chat_members a
                     JOIN chat_members b ON b.chat_id = a.chat_id
                     WHERE a.user_id = ?1 AND b.user_id = ?2
                     LIMIT 1",
                    params![user_a, user_b],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| RepoError::internal(format!("has_shared_chat: {e}")))?;
            Ok(found.is_some())
        })
        .await
    }

    async fn add_chat_members(
        &self,
        chat_id: &str,
        members: &[MemberRecord],
        max_members: usize,
        events: &[NewSyncEvent],
        now_ms: u64,
    ) -> Result<Vec<(String, u64)>, RepoError> {
        let chat_id = chat_id.to_string();
        let members = members.to_vec();
        let events = events.to_vec();
        self.call(move |conn| {
            let tx = conn
                .transaction()
                .map_err(|e| RepoError::internal(format!("begin add_members: {e}")))?;
            let current: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM chat_members WHERE chat_id = ?1",
                    params![chat_id],
                    |row| row.get(0),
                )
                .map_err(|e| RepoError::internal(format!("count members: {e}")))?;
            if current as usize + members.len() > max_members {
                return Err(RepoError::Conflict("group_full".into()));
            }
            for member in &members {
                tx.execute(
                    "INSERT INTO chat_members(chat_id, user_id, joined_at_ms, role) VALUES(?1, ?2, ?3, ?4)",
                    params![
                        member.chat_id,
                        member.user_id,
                        member.joined_at_ms as i64,
                        member.role as i64
                    ],
                )
                .map_err(|e| map_sql("insert member", e))?;
            }
            bump_chat_updated_at(&tx, &chat_id, now_ms)?;
            let seqs = insert_events(&tx, &events)?;
            tx.commit()
                .map_err(|e| RepoError::internal(format!("commit add_members: {e}")))?;
            Ok(seqs)
        })
        .await
    }

    async fn remove_chat_member(
        &self,
        chat_id: &str,
        user_id: &str,
        transfer_owner_to: Option<&str>,
        events: &[NewSyncEvent],
        now_ms: u64,
    ) -> Result<Vec<(String, u64)>, RepoError> {
        let chat_id = chat_id.to_string();
        let user_id = user_id.to_string();
        let transfer_owner_to = transfer_owner_to.map(str::to_string);
        let events = events.to_vec();
        self.call(move |conn| {
            let tx = conn
                .transaction()
                .map_err(|e| RepoError::internal(format!("begin remove_member: {e}")))?;
            let changed = tx
                .execute(
                    "DELETE FROM chat_members WHERE chat_id = ?1 AND user_id = ?2",
                    params![chat_id, user_id],
                )
                .map_err(|e| map_sql("delete member", e))?;
            if changed == 0 {
                return Err(RepoError::NotFound(format!("member {user_id}")));
            }
            if let Some(new_owner) = &transfer_owner_to {
                let promoted = tx
                    .execute(
                        "UPDATE chat_members SET role = ?3 WHERE chat_id = ?1 AND user_id = ?2",
                        params![
                            chat_id,
                            new_owner,
                            rchat_proto::proto::ChatMemberRole::Owner as i64
                        ],
                    )
                    .map_err(|e| map_sql("promote owner", e))?;
                if promoted == 0 {
                    return Err(RepoError::NotFound(format!("member {new_owner}")));
                }
            }
            bump_chat_updated_at(&tx, &chat_id, now_ms)?;
            let seqs = insert_events(&tx, &events)?;
            tx.commit()
                .map_err(|e| RepoError::internal(format!("commit remove_member: {e}")))?;
            Ok(seqs)
        })
        .await
    }

    async fn rename_chat(
        &self,
        chat_id: &str,
        name: &str,
        events: &[NewSyncEvent],
        now_ms: u64,
    ) -> Result<Vec<(String, u64)>, RepoError> {
        let chat_id = chat_id.to_string();
        let name = name.to_string();
        let events = events.to_vec();
        self.call(move |conn| {
            let tx = conn
                .transaction()
                .map_err(|e| RepoError::internal(format!("begin rename_chat: {e}")))?;
            let changed = tx
                .execute(
                    "UPDATE chats SET name = ?2, updated_at_ms = MAX(updated_at_ms, ?3) WHERE chat_id = ?1",
                    params![chat_id, name, now_ms as i64],
                )
                .map_err(|e| map_sql("rename chat", e))?;
            if changed == 0 {
                return Err(RepoError::NotFound(format!("chat {chat_id}")));
            }
            let seqs = insert_events(&tx, &events)?;
            tx.commit()
                .map_err(|e| RepoError::internal(format!("commit rename_chat: {e}")))?;
            Ok(seqs)
        })
        .await
    }

    // --- сообщения ---

    async fn insert_message(&self, insert: &NewMessage) -> Result<InsertMessageOutcome, RepoError> {
        let insert = insert.clone();
        self.call(move |conn| {
            let message = &insert.message;
            let existing = find_by_idempotency(
                conn,
                &message.chat_id,
                &message.sender_device_id,
                &message.client_message_id,
            )?;
            if let Some(existing) = existing {
                return Ok(InsertMessageOutcome::Duplicate(existing));
            }
            let tx = conn
                .transaction()
                .map_err(|e| RepoError::internal(format!("begin insert_message: {e}")))?;
            insert_message_row(&tx, message)?;
            for recipient in &insert.recipients {
                insert_recipient(&tx, recipient)?;
            }
            tx.execute(
                "UPDATE chats SET last_server_id = MAX(last_server_id, ?2), updated_at_ms = MAX(updated_at_ms, ?3)
                 WHERE chat_id = ?1",
                params![
                    message.chat_id,
                    message.server_id as i64,
                    message.created_at_ms as i64
                ],
            )
            .map_err(|e| RepoError::internal(format!("bump chat: {e}")))?;
            let event_seqs = match insert_events(&tx, &insert.events) {
                Ok(seqs) => seqs,
                Err(RepoError::Conflict(_)) => {
                    // Гонка по idempotency key: уникальный индекс поймал повтор.
                    drop(tx);
                    let existing = find_by_idempotency(
                        conn,
                        &message.chat_id,
                        &message.sender_device_id,
                        &message.client_message_id,
                    )?
                    .ok_or_else(|| RepoError::internal("duplicate message vanished"))?;
                    return Ok(InsertMessageOutcome::Duplicate(existing));
                }
                Err(error) => return Err(error),
            };
            tx.commit()
                .map_err(|e| RepoError::internal(format!("commit insert_message: {e}")))?;
            Ok(InsertMessageOutcome::Inserted {
                message: insert.message.clone(),
                event_seqs,
            })
        })
        .await
    }

    async fn get_message(&self, server_id: u64) -> Result<Option<MessageRecord>, RepoError> {
        self.call(move |conn| get_message(conn, server_id)).await
    }

    async fn message_recipient(
        &self,
        server_id: u64,
        device_id: &str,
    ) -> Result<Option<RecipientRecord>, RepoError> {
        let device_id = device_id.to_string();
        self.call(move |conn| {
            let sql = format!(
                "SELECT {RECIPIENT_COLUMNS} FROM message_recipients
                 WHERE server_id = ?1 AND device_id = ?2"
            );
            conn.query_row(
                &sql,
                params![server_id as i64, device_id],
                recipient_from_row,
            )
            .optional()
            .map_err(|e| RepoError::internal(format!("message_recipient: {e}")))
        })
        .await
    }

    async fn recipients_for_device(
        &self,
        server_ids: &[u64],
        device_id: &str,
    ) -> Result<Vec<RecipientRecord>, RepoError> {
        if server_ids.is_empty() {
            return Ok(Vec::new());
        }
        let server_ids = server_ids.to_vec();
        let device_id = device_id.to_string();
        self.call(move |conn| {
            let placeholders: Vec<String> =
                (1..=server_ids.len()).map(|i| format!("?{i}")).collect();
            let sql = format!(
                "SELECT {RECIPIENT_COLUMNS} FROM message_recipients
                 WHERE device_id = ?{} AND server_id IN ({})
                 ORDER BY server_id DESC",
                server_ids.len() + 1,
                placeholders.join(", ")
            );
            let ids_i64: Vec<i64> = server_ids.iter().map(|id| *id as i64).collect();
            let mut params_vec: Vec<&dyn rusqlite::ToSql> = ids_i64
                .iter()
                .map(|id| id as &dyn rusqlite::ToSql)
                .collect();
            params_vec.push(&device_id);
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| RepoError::internal(format!("prepare recipients: {e}")))?;
            let rows = stmt
                .query_map(params_vec.as_slice(), recipient_from_row)
                .map_err(|e| RepoError::internal(format!("query recipients: {e}")))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| RepoError::internal(format!("rows recipients: {e}")))
        })
        .await
    }

    async fn list_history(
        &self,
        chat_id: &str,
        before_server_id: u64,
        limit: u32,
    ) -> Result<Vec<MessageRecord>, RepoError> {
        let chat_id = chat_id.to_string();
        self.call(move |conn| {
            let sql = format!(
                "SELECT {MESSAGE_COLUMNS} FROM messages
                 WHERE chat_id = ?1 AND (?2 = 0 OR server_id < ?2)
                 ORDER BY server_id DESC LIMIT ?3"
            );
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| RepoError::internal(format!("prepare history: {e}")))?;
            let rows = stmt
                .query_map(
                    params![chat_id, before_server_id as i64, (limit as i64) + 1],
                    message_from_row,
                )
                .map_err(|e| RepoError::internal(format!("query history: {e}")))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| RepoError::internal(format!("rows history: {e}")))
        })
        .await
    }

    async fn update_message(
        &self,
        server_id: u64,
        update: &MessageUpdate,
    ) -> Result<(MessageRecord, Vec<(String, u64)>), RepoError> {
        let update = update.clone();
        self.call(move |conn| {
            let current = get_message(conn, server_id)?
                .ok_or_else(|| RepoError::NotFound(format!("message {server_id}")))?;
            if current.sender_user_id != update.editor_user_id {
                return Err(RepoError::Conflict("not the author".into()));
            }
            if current.deleted_at_ms != 0 && update.deleted_at_ms == 0 {
                return Err(RepoError::Conflict("message has been deleted".into()));
            }
            let tx = conn
                .transaction()
                .map_err(|e| RepoError::internal(format!("begin update_message: {e}")))?;
            let edited_at = if update.edited_at_ms != 0 {
                update.edited_at_ms
            } else {
                current.edited_at_ms
            };
            let deleted_at = if update.deleted_at_ms != 0 {
                update.deleted_at_ms
            } else {
                current.deleted_at_ms
            };
            let content_kind = update
                .content_kind
                .clone()
                .unwrap_or_else(|| current.content_kind.clone());
            tx.execute(
                "UPDATE messages SET edited_at_ms = ?2, deleted_at_ms = ?3, content_kind = ?4,
                     sender_device_id = COALESCE(?5, sender_device_id),
                     sender_curve25519 = COALESCE(?6, sender_curve25519)
                 WHERE server_id = ?1",
                params![
                    server_id as i64,
                    edited_at as i64,
                    deleted_at as i64,
                    content_kind,
                    update.sender_device_id,
                    update.sender_curve25519
                ],
            )
            .map_err(|e| RepoError::internal(format!("update message: {e}")))?;
            // Политика удаления «для всех»: содержимое не остаётся на сервере.
            if update.clear_content {
                tx.execute(
                    "UPDATE messages SET plaintext_dev = NULL, group_ciphertext = NULL,
                         group_session_id = '' WHERE server_id = ?1",
                    params![server_id as i64],
                )
                .map_err(|e| RepoError::internal(format!("clear content: {e}")))?;
            } else {
                if let Some(text) = &update.plaintext_dev {
                    tx.execute(
                        "UPDATE messages SET plaintext_dev = ?2 WHERE server_id = ?1",
                        params![server_id as i64, text],
                    )
                    .map_err(|e| RepoError::internal(format!("update plaintext: {e}")))?;
                }
                if let Some(group) = &update.group_ciphertext {
                    tx.execute(
                        "UPDATE messages SET group_ciphertext = ?2 WHERE server_id = ?1",
                        params![server_id as i64, group],
                    )
                    .map_err(|e| RepoError::internal(format!("update group ciphertext: {e}")))?;
                }
                if let Some(session_id) = &update.group_session_id {
                    tx.execute(
                        "UPDATE messages SET group_session_id = ?2 WHERE server_id = ?1",
                        params![server_id as i64, session_id],
                    )
                    .map_err(|e| RepoError::internal(format!("update group session id: {e}")))?;
                }
            }
            if let Some(recipients) = &update.recipients {
                tx.execute(
                    "DELETE FROM message_recipients WHERE server_id = ?1",
                    params![server_id as i64],
                )
                .map_err(|e| RepoError::internal(format!("clear recipients: {e}")))?;
                for recipient in recipients {
                    insert_recipient(&tx, recipient)?;
                }
            }
            let seqs = insert_events(&tx, &update.events)?;
            tx.commit()
                .map_err(|e| RepoError::internal(format!("commit update_message: {e}")))?;
            let updated = get_message(conn, server_id)?
                .ok_or_else(|| RepoError::internal("updated message vanished"))?;
            Ok((updated, seqs))
        })
        .await
    }

    async fn max_message_server_id(&self) -> Result<u64, RepoError> {
        self.call(move |conn| {
            let max: i64 = conn
                .query_row(
                    "SELECT COALESCE(MAX(server_id), 0) FROM messages",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| RepoError::internal(format!("max server id: {e}")))?;
            Ok(max.max(0) as u64)
        })
        .await
    }

    // --- receipts ---

    async fn upsert_receipt(
        &self,
        receipt: &ReceiptRecord,
        events: &[NewSyncEvent],
    ) -> Result<(ReceiptUpdate, Vec<(String, u64)>), RepoError> {
        let receipt = receipt.clone();
        let events = events.to_vec();
        self.call(move |conn| {
            let old: Option<(u64, bool)> = conn
                .query_row(
                    "SELECT up_to_server_id, read FROM receipts WHERE chat_id = ?1 AND user_id = ?2",
                    params![receipt.chat_id, receipt.user_id],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?.max(0) as u64,
                            row.get::<_, i64>(1)? != 0,
                        ))
                    },
                )
                .optional()
                .map_err(|e| RepoError::internal(format!("read receipt: {e}")))?;
            let (old_up, old_read) = old.unwrap_or((0, false));
            let new_up = old_up.max(receipt.up_to_server_id);
            let new_read = old_read || receipt.read;
            let moved = new_up != old_up || new_read != old_read;
            let tx = conn
                .transaction()
                .map_err(|e| RepoError::internal(format!("begin receipt: {e}")))?;
            tx.execute(
                "INSERT INTO receipts(chat_id, user_id, up_to_server_id, read, updated_at_ms)
                 VALUES(?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(chat_id, user_id) DO UPDATE SET
                     up_to_server_id = excluded.up_to_server_id,
                     read = excluded.read,
                     updated_at_ms = excluded.updated_at_ms",
                params![
                    receipt.chat_id,
                    receipt.user_id,
                    new_up as i64,
                    if new_read { 1 } else { 0 },
                    receipt.updated_at_ms as i64
                ],
            )
            .map_err(|e| map_sql("upsert receipt", e))?;
            let seqs = if moved {
                insert_events(&tx, &events)?
            } else {
                Vec::new()
            };
            tx.commit()
                .map_err(|e| RepoError::internal(format!("commit receipt: {e}")))?;
            Ok((
                ReceiptUpdate {
                    moved,
                    up_to_server_id: new_up,
                    read: new_read,
                },
                seqs,
            ))
        })
        .await
    }

    // --- синхронизация ---

    async fn append_sync_events(
        &self,
        events: &[NewSyncEvent],
    ) -> Result<Vec<(String, u64)>, RepoError> {
        let events = events.to_vec();
        self.call(move |conn| {
            let tx = conn
                .transaction()
                .map_err(|e| RepoError::internal(format!("begin append_events: {e}")))?;
            let seqs = insert_events(&tx, &events)?;
            tx.commit()
                .map_err(|e| RepoError::internal(format!("commit append_events: {e}")))?;
            Ok(seqs)
        })
        .await
    }

    async fn fetch_sync_events(
        &self,
        device_id: &str,
        after_seq: u64,
        limit: u32,
        retention_ms: u64,
        keep_min: u32,
        now_ms: u64,
    ) -> Result<SyncFetchRecord, RepoError> {
        let device_id = device_id.to_string();
        self.call(move |conn| {
            let tx = conn
                .transaction()
                .map_err(|e| RepoError::internal(format!("begin fetch_events: {e}")))?;
            let max_seq: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(seq), 0) FROM sync_events WHERE device_id = ?1",
                    params![device_id],
                    |row| row.get(0),
                )
                .map_err(|e| RepoError::internal(format!("max seq: {e}")))?;
            let keep_from = (max_seq as u64).saturating_sub(keep_min as u64);
            let cutoff = now_ms.saturating_sub(retention_ms) as i64;
            tx.execute(
                "DELETE FROM sync_events
                 WHERE device_id = ?1 AND seq <= ?2 AND created_at_ms < ?3",
                params![device_id, keep_from as i64, cutoff],
            )
            .map_err(|e| RepoError::internal(format!("prune events: {e}")))?;
            let up_to: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(seq), 0) FROM sync_events WHERE device_id = ?1",
                    params![device_id],
                    |row| row.get(0),
                )
                .map_err(|e| RepoError::internal(format!("up_to seq: {e}")))?;
            let next_seq: Option<Option<i64>> = tx
                .query_row(
                    "SELECT MIN(seq) FROM sync_events WHERE device_id = ?1 AND seq > ?2",
                    params![device_id, after_seq as i64],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| RepoError::internal(format!("next seq: {e}")))?;
            let resync_required = next_seq
                .flatten()
                .map(|seq| seq.max(0) as u64 != after_seq.saturating_add(1))
                .unwrap_or(false);
            let sql = "SELECT device_id, seq, kind_hint, payload, created_at_ms
                       FROM sync_events WHERE device_id = ?1 AND seq > ?2
                       ORDER BY seq LIMIT ?3";
            let mut stmt = tx
                .prepare(sql)
                .map_err(|e| RepoError::internal(format!("prepare fetch_events: {e}")))?;
            let events: Vec<SyncEventRecord> = stmt
                .query_map(
                    params![device_id, after_seq as i64, limit as i64],
                    sync_event_from_row,
                )
                .map_err(|e| RepoError::internal(format!("query fetch_events: {e}")))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| RepoError::internal(format!("rows fetch_events: {e}")))?;
            drop(stmt);
            let min_seq: i64 = tx
                .query_row(
                    "SELECT COALESCE(MIN(seq), 0) FROM sync_events WHERE device_id = ?1",
                    params![device_id],
                    |row| row.get(0),
                )
                .map_err(|e| RepoError::internal(format!("min seq: {e}")))?;
            tx.commit()
                .map_err(|e| RepoError::internal(format!("commit fetch_events: {e}")))?;
            Ok(SyncFetchRecord {
                events,
                up_to_seq: up_to.max(0) as u64,
                min_seq: min_seq.max(0) as u64,
                resync_required,
            })
        })
        .await
    }

    async fn record_cursor(
        &self,
        device_id: &str,
        cursor: u64,
        now_ms: u64,
    ) -> Result<(), RepoError> {
        let device_id = device_id.to_string();
        self.call(move |conn| {
            conn.execute(
                "INSERT INTO device_sync_state(device_id, cursor, next_seq, updated_at_ms)
                 VALUES(?1, ?2, 1, ?3)
                 ON CONFLICT(device_id) DO UPDATE SET
                     cursor = MAX(cursor, excluded.cursor),
                     updated_at_ms = excluded.updated_at_ms",
                params![device_id, cursor as i64, now_ms as i64],
            )
            .map_err(|e| RepoError::internal(format!("record cursor: {e}")))?;
            Ok(())
        })
        .await
    }

    async fn max_seq_for_device(&self, device_id: &str) -> Result<u64, RepoError> {
        let device_id = device_id.to_string();
        self.call(move |conn| {
            let max: i64 = conn
                .query_row(
                    "SELECT COALESCE(MAX(seq), 0) FROM sync_events WHERE device_id = ?1",
                    params![device_id],
                    |row| row.get(0),
                )
                .map_err(|e| RepoError::internal(format!("max seq for device: {e}")))?;
            Ok(max.max(0) as u64)
        })
        .await
    }
}

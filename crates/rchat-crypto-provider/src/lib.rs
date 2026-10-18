//! E2E-провайдер на базе зрелой библиотеки [vodozemac] (реализация Olm
//! из Matrix, совместимая с libolm).
//!
//! Библиотека и протокол зафиксированы в `docs/architecture.md` и
//! `docs/threat-model.md`: Olm (двойной ratchet, X3DH-подобное установление
//! через one-time prekey), версия протокола `E2E_VERSION_OLM_V1`.
//!
//! Состояние (аккаунт и сессии) сериализуется только штатными pickle-формами
//! vodozemac, зашифрованными ключом из [`SecretStore`] (`crypto_pickle_key`),
//! и хранится в [`CryptoStateStore`] клиента. Ключ шифрования pickle и сами
//! pickle никогда не логируются.
//!
//! [vodozemac]: https://github.com/matrix-org/vodozemac

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use rchat_core::error::CryptoError;
use rchat_core::traits::{
    secret_keys, CryptoProvider, CryptoStateStore, EncryptedMessage, GroupEncryption, GroupKeyInfo,
    KeyUpload, OneTimeKey, PeerDevice, SecretStore,
};
use rchat_proto::proto::OlmMessageType;
use tokio::sync::{Mutex, MutexGuard};
use vodozemac::megolm::{
    GroupSession, GroupSessionPickle, InboundGroupSession, InboundGroupSessionPickle,
    MegolmMessage, SessionConfig as MegolmSessionConfig, SessionKey,
};
use vodozemac::olm::{
    Account, AccountPickle, OlmMessage, Session, SessionConfig, SessionCreationError, SessionPickle,
};
use vodozemac::{base64_decode, Curve25519PublicKey, KeyId};
use zeroize::Zeroizing;

/// Ключ аккаунта в [`CryptoStateStore`].
const ACCOUNT_STATE_KEY: &str = "olm_account";
/// Префикс ключей сессий: `olm_session:<device_id>`.
const SESSION_STATE_PREFIX: &str = "olm_session:";
/// Префикс ключей исходящих megolm-сессий: `megolm_out:<chat_id>`.
const GROUP_OUT_STATE_PREFIX: &str = "megolm_out:";
/// Префикс ключей входящих megolm-сессий: `megolm_in:<session_id>`.
const GROUP_IN_STATE_PREFIX: &str = "megolm_in:";
/// Длина ключа шифрования pickle (требование vodozemac).
const PICKLE_KEY_LEN: usize = 32;
/// Магия внутреннего payload ключа групповой сессии (никогда не покидает
/// Olm-конверт в открытом виде).
const GROUP_KEY_PAYLOAD_MAGIC: &[u8; 4] = b"RMGK";
/// Версия формата payload ключа групповой сессии.
const GROUP_KEY_PAYLOAD_VERSION: u8 = 1;

/// E2E-провайдер Olm + Megolm.
pub struct VodozemacCryptoProvider {
    store: Arc<dyn CryptoStateStore>,
    secrets: Arc<dyn SecretStore>,
    inner: Mutex<Option<Loaded>>,
}

/// Загруженное криптографическое состояние.
struct Loaded {
    account: Account,
    /// Ключ шифрования pickle; zeroize при выгрузке.
    pickle_key: Zeroizing<[u8; PICKLE_KEY_LEN]>,
    /// Установленные сессии по `device_id` собеседника.
    sessions: HashMap<String, Session>,
    /// Исходящие megolm-сессии по `chat_id`.
    group_out: HashMap<String, GroupSession>,
    /// Входящие megolm-сессии по `session_id`.
    group_in: HashMap<String, LoadedInbound>,
}

/// Входящая megolm-сессия с чатом, к которому она привязана: сессия одного
/// чата не может расшифровывать сообщения другого.
struct LoadedInbound {
    chat_id: String,
    session: InboundGroupSession,
}

impl VodozemacCryptoProvider {
    pub fn new(store: Arc<dyn CryptoStateStore>, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            store,
            secrets,
            inner: Mutex::new(None),
        }
    }

    /// Возвращает загруженное состояние, загружая его при первом обращении.
    async fn loaded(&self) -> Result<MutexGuard<'_, Option<Loaded>>, CryptoError> {
        let mut guard = self.inner.lock().await;
        if guard.is_none() {
            let pickle_key = load_or_create_pickle_key(self.secrets.as_ref())?;
            let account = match self
                .store
                .crypto_load(ACCOUNT_STATE_KEY)
                .await
                .map_err(state_error)?
            {
                Some(bytes) => {
                    let text = String::from_utf8(bytes)
                        .map_err(|_| CryptoError::State("account pickle is not utf-8".into()))?;
                    let pickle = AccountPickle::from_encrypted(&text, &pickle_key)
                        .map_err(|e| CryptoError::State(format!("account pickle: {e}")))?;
                    Account::from(pickle)
                }
                None => {
                    let account = Account::new();
                    save_account(self.store.as_ref(), &account, &pickle_key).await?;
                    account
                }
            };
            *guard = Some(Loaded {
                account,
                pickle_key,
                sessions: HashMap::new(),
                group_out: HashMap::new(),
                group_in: HashMap::new(),
            });
        }
        Ok(guard)
    }
}

#[async_trait]
impl CryptoProvider for VodozemacCryptoProvider {
    fn e2e_version(&self) -> u32 {
        rchat_proto::E2E_VERSION_OLM_V1
    }

    fn protocol_name(&self) -> &'static str {
        "olm.v1"
    }

    async fn initialize(&self) -> Result<(), CryptoError> {
        let _ = self.loaded().await?;
        Ok(())
    }

    async fn identity_keys(&self) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
        let guard = self.loaded().await?;
        let loaded = guard.as_ref().expect("loaded above");
        Ok((
            loaded.account.ed25519_key().as_bytes().to_vec(),
            loaded.account.curve25519_key().to_bytes().to_vec(),
        ))
    }

    async fn keys_for_upload(&self, want_one_time: usize) -> Result<KeyUpload, CryptoError> {
        let mut guard = self.loaded().await?;
        let loaded = guard.as_mut().expect("loaded above");

        let stored = loaded.account.stored_one_time_key_count();
        if stored < want_one_time {
            loaded
                .account
                .generate_one_time_keys(want_one_time - stored);
        }
        if loaded.account.fallback_key().is_empty() {
            // Резервный ключ нужен серверу, когда одноразовые закончились.
            // Предыдущий приватный ключ vodozemac хранит для расшифровки.
            loaded.account.generate_fallback_key();
        }

        let mut upload = KeyUpload::default();
        for (key_id, public_key) in loaded.account.one_time_keys() {
            upload.one_time_keys.push(OneTimeKey {
                key_id: key_id_to_u32(key_id)?,
                public_key: public_key.to_bytes().to_vec(),
                fallback: false,
            });
        }
        if let Some((key_id, public_key)) = loaded.account.fallback_key().into_iter().next() {
            upload.fallback_key = Some(OneTimeKey {
                key_id: fallback_key_id_to_u32(key_id)?,
                public_key: public_key.to_bytes().to_vec(),
                fallback: true,
            });
        }

        // Сгенерированные приватные ключи должны пережить перезапуск клиента
        // до получения подтверждения сервера.
        save_account(self.store.as_ref(), &loaded.account, &loaded.pickle_key).await?;
        Ok(upload)
    }

    async fn mark_published(&self) -> Result<(), CryptoError> {
        let mut guard = self.loaded().await?;
        let loaded = guard.as_mut().expect("loaded above");
        loaded.account.mark_keys_as_published();
        save_account(self.store.as_ref(), &loaded.account, &loaded.pickle_key).await?;
        Ok(())
    }

    async fn has_session(&self, device_id: &str) -> Result<bool, CryptoError> {
        let mut guard = self.loaded().await?;
        let loaded = guard.as_mut().expect("loaded above");
        if loaded.sessions.contains_key(device_id) {
            return Ok(true);
        }
        let session = load_session(self.store.as_ref(), loaded, device_id).await?;
        let present = session.is_some();
        if let Some(session) = session {
            loaded.sessions.insert(device_id.to_string(), session);
        }
        Ok(present)
    }

    async fn encrypt_for(
        &self,
        targets: &[PeerDevice],
        plaintext: &[u8],
    ) -> Result<Vec<EncryptedMessage>, CryptoError> {
        let mut guard = self.loaded().await?;
        let loaded = guard.as_mut().expect("loaded above");
        let own_curve25519 = loaded.account.curve25519_key().to_bytes().to_vec();
        let mut encrypted = Vec::with_capacity(targets.len());

        for target in targets {
            let mut session = match loaded.sessions.remove(&target.device_id) {
                Some(session) => session,
                None => {
                    let one_time_key = target
                        .one_time_key
                        .as_ref()
                        .ok_or_else(|| CryptoError::MissingPreKey(target.device_id.clone()))?;
                    let identity =
                        Curve25519PublicKey::from_slice(&target.curve25519).map_err(|e| {
                            CryptoError::Session(
                                target.device_id.clone(),
                                format!("identity key: {e}"),
                            )
                        })?;
                    let one_time = Curve25519PublicKey::from_slice(&one_time_key.public_key)
                        .map_err(|e| {
                            CryptoError::MissingPreKey(format!(
                                "{} (ключ сервера некорректен: {e})",
                                target.device_id
                            ))
                        })?;
                    loaded
                        .account
                        .create_outbound_session(SessionConfig::default(), identity, one_time)
                        .map_err(|e| {
                            CryptoError::Session(target.device_id.clone(), e.to_string())
                        })?
                }
            };
            let message = session
                .encrypt(plaintext)
                .map_err(|e| CryptoError::Session(target.device_id.clone(), e.to_string()))?;
            loaded.sessions.insert(target.device_id.clone(), session);
            // Ratchet изменился — сохраняем до следующей попытки отправки.
            save_session(self.store.as_ref(), loaded, &target.device_id).await?;

            let (olm_type, ciphertext) = encode_message(message);
            encrypted.push(EncryptedMessage {
                device_id: target.device_id.clone(),
                ciphertext,
                e2e_version: rchat_proto::E2E_VERSION_OLM_V1,
                olm_type,
                sender_curve25519: own_curve25519.clone(),
            });
        }
        Ok(encrypted)
    }

    async fn decrypt_from(
        &self,
        sender: &PeerDevice,
        message: &EncryptedMessage,
    ) -> Result<Vec<u8>, CryptoError> {
        let mut guard = self.loaded().await?;
        let loaded = guard.as_mut().expect("loaded above");
        let olm_message = decode_message(message.olm_type, &message.ciphertext)
            .map_err(|e| CryptoError::DecryptFailed(sender.device_id.clone(), e))?;
        let identity = Curve25519PublicKey::from_slice(&sender.curve25519).map_err(|e| {
            CryptoError::DecryptFailed(sender.device_id.clone(), format!("identity key: {e}"))
        })?;

        match olm_message {
            OlmMessage::PreKey(pre_key) => {
                // Проверка подписи prekey-сообщения и соответствия ключа
                // отправителя выполняется внутри vodozemac.
                match loaded.account.create_inbound_session(
                    SessionConfig::default(),
                    identity,
                    &pre_key,
                ) {
                    Ok(result) => {
                        // Одноразовый ключ израсходован: аккаунт изменился.
                        save_account(self.store.as_ref(), &loaded.account, &loaded.pickle_key)
                            .await?;
                        loaded
                            .sessions
                            .insert(sender.device_id.clone(), result.session);
                        save_session(self.store.as_ref(), loaded, &sender.device_id).await?;
                        Ok(result.plaintext)
                    }
                    Err(SessionCreationError::MissingOneTimeKey(_)) => {
                        // Отправитель ещё не получил наш ответ и повторил
                        // prekey-сообщение: ключ израсходован, но сессия
                        // установлена — расшифровываем её ratchet.
                        decrypt_with_session(
                            self.store.as_ref(),
                            loaded,
                            sender,
                            OlmMessage::PreKey(pre_key),
                        )
                        .await
                    }
                    Err(SessionCreationError::MismatchedIdentityKey(..)) => {
                        Err(CryptoError::IdentityMismatch(sender.device_id.clone()))
                    }
                    Err(e) => Err(CryptoError::Session(
                        sender.device_id.clone(),
                        e.to_string(),
                    )),
                }
            }
            OlmMessage::Normal(normal) => {
                decrypt_with_session(
                    self.store.as_ref(),
                    loaded,
                    sender,
                    OlmMessage::Normal(normal),
                )
                .await
            }
        }
    }

    async fn remove_session(&self, device_id: &str) -> Result<(), CryptoError> {
        let mut guard = self.loaded().await?;
        let loaded = guard.as_mut().expect("loaded above");
        loaded.sessions.remove(device_id);
        self.store
            .crypto_delete(&session_state_key(device_id))
            .await
            .map_err(state_error)?;
        Ok(())
    }

    async fn group_encrypt(
        &self,
        chat_id: &str,
        plaintext: &[u8],
        share_key: bool,
    ) -> Result<GroupEncryption, CryptoError> {
        let mut guard = self.loaded().await?;
        let loaded = guard.as_mut().expect("loaded above");
        let mut session = match loaded.group_out.remove(chat_id) {
            Some(session) => session,
            None => match load_group_out(self.store.as_ref(), loaded, chat_id).await? {
                Some(session) => session,
                None => GroupSession::new(MegolmSessionConfig::default()),
            },
        };
        // Ключ берём до encrypt: он должен соответствовать ратчету, с
        // которого начинается именно это сообщение.
        let session_key = share_key.then(|| session.session_key().to_bytes());
        let session_id = session.session_id();
        let message = session.encrypt(plaintext);
        loaded.group_out.insert(chat_id.to_string(), session);
        save_group_out(self.store.as_ref(), loaded, chat_id).await?;
        Ok(GroupEncryption {
            session_id,
            ciphertext: message.to_bytes(),
            session_key,
        })
    }

    async fn encrypt_group_key(
        &self,
        chat_id: &str,
        session_key: &[u8],
        targets: &[PeerDevice],
    ) -> Result<Vec<EncryptedMessage>, CryptoError> {
        let payload = build_group_key_payload(chat_id, session_key)?;
        self.encrypt_for(targets, &payload).await
    }

    async fn apply_group_key(
        &self,
        sender: &PeerDevice,
        message: &EncryptedMessage,
        expected_chat_id: &str,
    ) -> Result<GroupKeyInfo, CryptoError> {
        // Сначала Olm: содержимое конверта — внутренний payload провайдера.
        let payload = self.decrypt_from(sender, message).await?;
        let (chat_id, session_key) = parse_group_key_payload(&payload)
            .map_err(|reason| CryptoError::DecryptFailed(sender.device_id.clone(), reason))?;
        if chat_id != expected_chat_id {
            return Err(CryptoError::DecryptFailed(
                sender.device_id.clone(),
                "group key belongs to another chat".into(),
            ));
        }
        let inbound = InboundGroupSession::new(&session_key, MegolmSessionConfig::default());
        let session_id = inbound.session_id();

        let mut guard = self.loaded().await?;
        let loaded = guard.as_mut().expect("loaded above");
        // Повторная доставка ключа (или уже импортированный ранее) ничего
        // не меняет: ратчет не откатывается назад.
        if loaded.group_in.contains_key(&session_id)
            || load_group_in(self.store.as_ref(), loaded, &session_id)
                .await?
                .is_some()
        {
            return Ok(GroupKeyInfo {
                chat_id,
                session_id,
            });
        }
        loaded.group_in.insert(
            session_id.clone(),
            LoadedInbound {
                chat_id: chat_id.clone(),
                session: inbound,
            },
        );
        save_group_in(self.store.as_ref(), loaded, &session_id).await?;
        Ok(GroupKeyInfo {
            chat_id,
            session_id,
        })
    }

    async fn decrypt_group(
        &self,
        chat_id: &str,
        session_id: &str,
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        let message = MegolmMessage::from_bytes(ciphertext).map_err(|e| {
            CryptoError::DecryptFailed(session_id.to_string(), format!("decode megolm: {e}"))
        })?;
        let mut guard = self.loaded().await?;
        let loaded = guard.as_mut().expect("loaded above");
        if !loaded.group_in.contains_key(session_id) {
            if let Some(inbound) = load_group_in(self.store.as_ref(), loaded, session_id).await? {
                loaded.group_in.insert(session_id.to_string(), inbound);
            }
        }
        let plaintext = {
            let inbound = loaded.group_in.get_mut(session_id).ok_or_else(|| {
                CryptoError::DecryptFailed(
                    session_id.to_string(),
                    "no inbound group session".into(),
                )
            })?;
            if inbound.chat_id != chat_id {
                return Err(CryptoError::DecryptFailed(
                    session_id.to_string(),
                    "group session belongs to another chat".into(),
                ));
            }
            inbound
                .session
                .decrypt(&message)
                .map_err(|e| CryptoError::DecryptFailed(session_id.to_string(), e.to_string()))?
                .plaintext
        };
        // Ратчет входящей сессии продвинулся: фиксируем до подтверждения
        // применения сообщения.
        save_group_in(self.store.as_ref(), loaded, session_id).await?;
        Ok(plaintext)
    }

    async fn rotate_group_session(&self, chat_id: &str) -> Result<(), CryptoError> {
        let mut guard = self.loaded().await?;
        let loaded = guard.as_mut().expect("loaded above");
        loaded.group_out.remove(chat_id);
        self.store
            .crypto_delete(&group_out_state_key(chat_id))
            .await
            .map_err(state_error)?;
        Ok(())
    }

    async fn wipe(&self) -> Result<(), CryptoError> {
        let mut guard = self.loaded().await?;
        *guard = None;
        drop(guard);
        self.store
            .crypto_delete(ACCOUNT_STATE_KEY)
            .await
            .map_err(state_error)?;
        self.store
            .crypto_delete_prefix(SESSION_STATE_PREFIX)
            .await
            .map_err(state_error)?;
        self.store
            .crypto_delete_prefix(GROUP_OUT_STATE_PREFIX)
            .await
            .map_err(state_error)?;
        self.store
            .crypto_delete_prefix(GROUP_IN_STATE_PREFIX)
            .await
            .map_err(state_error)?;
        match self.secrets.delete(secret_keys::CRYPTO_PICKLE_KEY) {
            Ok(()) | Err(rchat_core::error::SecretStoreError::NotFound(_)) => Ok(()),
            Err(e) => Err(state_error(e)),
        }
    }
}

/// Ключ состояния сессии устройства.
fn session_state_key(device_id: &str) -> String {
    format!("{SESSION_STATE_PREFIX}{device_id}")
}

fn state_error(error: impl std::fmt::Display) -> CryptoError {
    CryptoError::State(error.to_string())
}

/// Кодирует сообщение Olm в пару (тип протокола, байты).
fn encode_message(message: OlmMessage) -> (OlmMessageType, Vec<u8>) {
    match message {
        OlmMessage::PreKey(message) => (OlmMessageType::Prekey, message.to_bytes()),
        OlmMessage::Normal(message) => (OlmMessageType::Normal, message.to_bytes()),
    }
}

/// Декодирует сообщение по типу протокола.
fn decode_message(kind: OlmMessageType, ciphertext: &[u8]) -> Result<OlmMessage, String> {
    // Соответствие типов: vodozemac использует 0 = PreKey, 1 = Normal,
    // протокол rchat — 0 = NORMAL, 1 = PREKEY.
    let parts_type = match kind {
        OlmMessageType::Prekey => 0,
        OlmMessageType::Normal => 1,
    };
    OlmMessage::from_parts(parts_type, ciphertext).map_err(|e| format!("decode olm message: {e}"))
}

/// Идентификатор ключа для протокола: младшие 32 бита счётчика vodozemac.
/// Сервер использует его только как метку при выдаче/удалении ключа.
fn key_id_to_u32(key_id: KeyId) -> Result<u32, CryptoError> {
    let bytes = base64_decode(key_id.to_base64())
        .map_err(|e| CryptoError::Internal(format!("key id encoding: {e}")))?;
    let mut buf = [0u8; 8];
    if bytes.len() != buf.len() {
        return Err(CryptoError::Internal("unexpected key id length".into()));
    }
    buf.copy_from_slice(&bytes);
    Ok(u64::from_be_bytes(buf) as u32)
}

/// Старший бит пространства идентификаторов резервного ключа.
///
/// В vodozemac счётчики одноразовых и резервных ключей независимы, поэтому
/// их младшие 32 бита пересекаются; сервер же хранит оба вида ключей в одной
/// таблице с уникальностью `(device_id, key_id)`.
const FALLBACK_KEY_ID_BIT: u32 = 0x8000_0000;

/// Идентификатор резервного ключа: отдельное пространство, чтобы не
/// конфликтовать с одноразовыми ключами одного устройства.
fn fallback_key_id_to_u32(key_id: KeyId) -> Result<u32, CryptoError> {
    Ok(key_id_to_u32(key_id)? | FALLBACK_KEY_ID_BIT)
}

/// Читает ключ шифрования pickle из хранилища секретов, создавая при первом
/// запуске. Ключ никогда не логируется и обнуляется при выгрузке.
fn load_or_create_pickle_key(
    secrets: &dyn SecretStore,
) -> Result<Zeroizing<[u8; PICKLE_KEY_LEN]>, CryptoError> {
    match secrets.get(secret_keys::CRYPTO_PICKLE_KEY) {
        Ok(Some(bytes)) => {
            let key: [u8; PICKLE_KEY_LEN] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| CryptoError::State("crypto_pickle_key has invalid length".into()))?;
            Ok(Zeroizing::new(key))
        }
        Ok(None) => {
            let mut key = [0u8; PICKLE_KEY_LEN];
            getrandom::fill(&mut key)
                .map_err(|e| CryptoError::State(format!("random source: {e}")))?;
            secrets
                .set(secret_keys::CRYPTO_PICKLE_KEY, &key)
                .map_err(state_error)?;
            Ok(Zeroizing::new(key))
        }
        Err(e) => Err(state_error(e)),
    }
}

/// Штатный pickle аккаунта, зашифрованный ключом из SecretStore.
async fn save_account(
    store: &dyn CryptoStateStore,
    account: &Account,
    pickle_key: &[u8; PICKLE_KEY_LEN],
) -> Result<(), CryptoError> {
    let pickle = account.pickle().encrypt(pickle_key);
    store
        .crypto_save(ACCOUNT_STATE_KEY, pickle.as_bytes())
        .await
        .map_err(state_error)
}

/// Штатный pickle сессии.
async fn save_session(
    store: &dyn CryptoStateStore,
    loaded: &Loaded,
    device_id: &str,
) -> Result<(), CryptoError> {
    let Some(session) = loaded.sessions.get(device_id) else {
        return Ok(());
    };
    let pickle = session.pickle().encrypt(&loaded.pickle_key);
    store
        .crypto_save(&session_state_key(device_id), pickle.as_bytes())
        .await
        .map_err(state_error)
}

/// Загружает сессию устройства из хранилища, если она есть.
async fn load_session(
    store: &dyn CryptoStateStore,
    loaded: &Loaded,
    device_id: &str,
) -> Result<Option<Session>, CryptoError> {
    let Some(bytes) = store
        .crypto_load(&session_state_key(device_id))
        .await
        .map_err(state_error)?
    else {
        return Ok(None);
    };
    let text = String::from_utf8(bytes)
        .map_err(|_| CryptoError::State("session pickle is not utf-8".into()))?;
    let pickle = SessionPickle::from_encrypted(&text, &loaded.pickle_key)
        .map_err(|e| CryptoError::State(format!("session pickle: {e}")))?;
    Ok(Some(Session::from(pickle)))
}

/// Ключ состояния исходящей megolm-сессии чата.
fn group_out_state_key(chat_id: &str) -> String {
    format!("{GROUP_OUT_STATE_PREFIX}{chat_id}")
}

/// Ключ состояния входящей megolm-сессии.
fn group_in_state_key(session_id: &str) -> String {
    format!("{GROUP_IN_STATE_PREFIX}{session_id}")
}

/// Внутренний payload ключа групповой сессии:
/// `MAGIC(4) | version(1) | chat_len(u16 BE) | chat_id | key_len(u16 BE) | key`.
fn build_group_key_payload(chat_id: &str, session_key: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let chat = chat_id.as_bytes();
    let chat_len = u16::try_from(chat.len())
        .map_err(|_| CryptoError::Internal("chat id too long for group key payload".into()))?;
    let key_len = u16::try_from(session_key.len())
        .map_err(|_| CryptoError::Internal("session key too long for group key payload".into()))?;
    let mut payload = Vec::with_capacity(
        GROUP_KEY_PAYLOAD_MAGIC.len() + 1 + 2 + chat.len() + 2 + session_key.len(),
    );
    payload.extend_from_slice(GROUP_KEY_PAYLOAD_MAGIC);
    payload.push(GROUP_KEY_PAYLOAD_VERSION);
    payload.extend_from_slice(&chat_len.to_be_bytes());
    payload.extend_from_slice(chat);
    payload.extend_from_slice(&key_len.to_be_bytes());
    payload.extend_from_slice(session_key);
    Ok(payload)
}

/// Разбирает payload ключа групповой сессии. Ошибка — строка-причина:
/// payload приходит от недоверенной стороны.
fn parse_group_key_payload(payload: &[u8]) -> Result<(String, SessionKey), String> {
    let invalid = || "invalid group key payload".to_string();
    let header = GROUP_KEY_PAYLOAD_MAGIC.len() + 1;
    if payload.len() < header + 2
        || &payload[..GROUP_KEY_PAYLOAD_MAGIC.len()] != GROUP_KEY_PAYLOAD_MAGIC
    {
        return Err(invalid());
    }
    if payload[GROUP_KEY_PAYLOAD_MAGIC.len()] != GROUP_KEY_PAYLOAD_VERSION {
        return Err("unsupported group key payload version".into());
    }
    let mut cursor = header;
    let chat_len = read_u16(payload, &mut cursor).ok_or_else(invalid)? as usize;
    let chat = payload.get(cursor..cursor + chat_len).ok_or_else(invalid)?;
    let chat_id = String::from_utf8(chat.to_vec()).map_err(|_| invalid())?;
    if chat_id.is_empty() {
        return Err(invalid());
    }
    cursor += chat_len;
    let key_len = read_u16(payload, &mut cursor).ok_or_else(invalid)? as usize;
    let key = payload.get(cursor..cursor + key_len).ok_or_else(invalid)?;
    let session_key = SessionKey::from_bytes(key).map_err(|e| format!("session key: {e}"))?;
    Ok((chat_id, session_key))
}

fn read_u16(bytes: &[u8], cursor: &mut usize) -> Option<u16> {
    let end = *cursor + 2;
    let slice = bytes.get(*cursor..end)?;
    *cursor = end;
    Some(u16::from_be_bytes([slice[0], slice[1]]))
}

/// Штатный pickle исходящей megolm-сессии, зашифрованный ключом SecretStore.
async fn save_group_out(
    store: &dyn CryptoStateStore,
    loaded: &Loaded,
    chat_id: &str,
) -> Result<(), CryptoError> {
    let Some(session) = loaded.group_out.get(chat_id) else {
        return Ok(());
    };
    let pickle = session.pickle().encrypt(&loaded.pickle_key);
    store
        .crypto_save(&group_out_state_key(chat_id), pickle.as_bytes())
        .await
        .map_err(state_error)
}

/// Загружает исходящую megolm-сессию чата из хранилища, если она есть.
async fn load_group_out(
    store: &dyn CryptoStateStore,
    loaded: &Loaded,
    chat_id: &str,
) -> Result<Option<GroupSession>, CryptoError> {
    let Some(bytes) = store
        .crypto_load(&group_out_state_key(chat_id))
        .await
        .map_err(state_error)?
    else {
        return Ok(None);
    };
    let text = String::from_utf8(bytes)
        .map_err(|_| CryptoError::State("group session pickle is not utf-8".into()))?;
    let pickle = GroupSessionPickle::from_encrypted(&text, &loaded.pickle_key)
        .map_err(|e| CryptoError::State(format!("group session pickle: {e}")))?;
    Ok(Some(GroupSession::from_pickle(pickle)))
}

/// Входящая сессия хранится как `chat_id \n pickle`: сам pickle не помнит,
/// к какому чату относится ключ, а привязка обязательна.
async fn save_group_in(
    store: &dyn CryptoStateStore,
    loaded: &Loaded,
    session_id: &str,
) -> Result<(), CryptoError> {
    let Some(inbound) = loaded.group_in.get(session_id) else {
        return Ok(());
    };
    let pickle = inbound.session.pickle().encrypt(&loaded.pickle_key);
    let mut value = inbound.chat_id.clone().into_bytes();
    value.push(b'\n');
    value.extend_from_slice(pickle.as_bytes());
    store
        .crypto_save(&group_in_state_key(session_id), &value)
        .await
        .map_err(state_error)
}

/// Загружает входящую megolm-сессию из хранилища (вместе с привязкой к чату).
async fn load_group_in(
    store: &dyn CryptoStateStore,
    loaded: &Loaded,
    session_id: &str,
) -> Result<Option<LoadedInbound>, CryptoError> {
    let Some(bytes) = store
        .crypto_load(&group_in_state_key(session_id))
        .await
        .map_err(state_error)?
    else {
        return Ok(None);
    };
    let split = bytes.iter().position(|b| *b == b'\n').ok_or_else(|| {
        CryptoError::State("inbound group session pickle has no chat binding".into())
    })?;
    let chat_id = String::from_utf8(bytes[..split].to_vec())
        .map_err(|_| CryptoError::State("inbound group chat id is not utf-8".into()))?;
    let text = String::from_utf8(bytes[split + 1..].to_vec())
        .map_err(|_| CryptoError::State("inbound group session pickle is not utf-8".into()))?;
    let pickle = InboundGroupSessionPickle::from_encrypted(&text, &loaded.pickle_key)
        .map_err(|e| CryptoError::State(format!("inbound group session pickle: {e}")))?;
    Ok(Some(LoadedInbound {
        chat_id,
        session: InboundGroupSession::from_pickle(pickle),
    }))
}

/// Расшифровывает сообщение существующей сессией устройства и сохраняет
/// изменившийся ratchet.
async fn decrypt_with_session(
    store: &dyn CryptoStateStore,
    loaded: &mut Loaded,
    sender: &PeerDevice,
    message: OlmMessage,
) -> Result<Vec<u8>, CryptoError> {
    let mut session = match loaded.sessions.remove(&sender.device_id) {
        Some(session) => session,
        None => load_session(store, loaded, &sender.device_id)
            .await?
            .ok_or_else(|| {
                CryptoError::DecryptFailed(
                    sender.device_id.clone(),
                    "no established session".into(),
                )
            })?,
    };
    let plaintext = session
        .decrypt(&message)
        .map_err(|e| CryptoError::DecryptFailed(sender.device_id.clone(), e.to_string()))?;
    loaded.sessions.insert(sender.device_id.clone(), session);
    save_session(store, loaded, &sender.device_id).await?;
    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rchat_core::error::SecretStoreError;
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct MemStore {
        values: StdMutex<HashMap<String, Vec<u8>>>,
    }

    #[async_trait]
    impl CryptoStateStore for MemStore {
        async fn crypto_load(
            &self,
            key: &str,
        ) -> Result<Option<Vec<u8>>, rchat_core::error::StorageError> {
            Ok(self.values.lock().expect("poisoned").get(key).cloned())
        }
        async fn crypto_save(
            &self,
            key: &str,
            value: &[u8],
        ) -> Result<(), rchat_core::error::StorageError> {
            self.values
                .lock()
                .expect("poisoned")
                .insert(key.to_string(), value.to_vec());
            Ok(())
        }
        async fn crypto_delete(&self, key: &str) -> Result<(), rchat_core::error::StorageError> {
            self.values.lock().expect("poisoned").remove(key);
            Ok(())
        }
        async fn crypto_delete_prefix(
            &self,
            prefix: &str,
        ) -> Result<u64, rchat_core::error::StorageError> {
            let mut values = self.values.lock().expect("poisoned");
            let keys: Vec<String> = values
                .keys()
                .filter(|key| key.starts_with(prefix))
                .cloned()
                .collect();
            for key in &keys {
                values.remove(key);
            }
            Ok(keys.len() as u64)
        }
    }

    #[derive(Default)]
    struct MemSecrets {
        values: StdMutex<HashMap<String, Vec<u8>>>,
    }

    impl SecretStore for MemSecrets {
        fn get(&self, key: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
            Ok(self.values.lock().expect("poisoned").get(key).cloned())
        }
        fn set(&self, key: &str, value: &[u8]) -> Result<(), SecretStoreError> {
            self.values
                .lock()
                .expect("poisoned")
                .insert(key.to_string(), value.to_vec());
            Ok(())
        }
        fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
            self.values.lock().expect("poisoned").remove(key);
            Ok(())
        }
    }

    fn provider() -> VodozemacCryptoProvider {
        VodozemacCryptoProvider::new(
            Arc::new(MemStore::default()),
            Arc::new(MemSecrets::default()),
        )
    }

    /// Провайдер, переживающий «перезапуск» (новый экземпляр, те же хранилища).
    fn provider_with(store: Arc<MemStore>, secrets: Arc<MemSecrets>) -> VodozemacCryptoProvider {
        VodozemacCryptoProvider::new(store, secrets)
    }

    async fn peer(provider: &VodozemacCryptoProvider, device_id: &str) -> (PeerDevice, KeyUpload) {
        provider.initialize().await.expect("init");
        let (ed25519, curve25519) = provider.identity_keys().await.expect("identity");
        let upload = provider.keys_for_upload(5).await.expect("keys");
        provider.mark_published().await.expect("publish");
        (
            PeerDevice {
                user_id: "u1".to_string(),
                device_id: device_id.to_string(),
                ed25519,
                curve25519,
                one_time_key: None,
            },
            upload,
        )
    }

    #[tokio::test]
    async fn round_trip_prekey_and_normal() {
        let alice = provider();
        let bob = provider();
        let (mut alice_peer, alice_keys) = peer(&alice, "a1").await;
        let (bob_peer, _) = peer(&bob, "b1").await;

        // Bob пишет Alice: первое сообщение — prekey.
        alice_peer.one_time_key = Some(alice_keys.one_time_keys[0].clone());
        let encrypted = bob
            .encrypt_for(&[alice_peer.clone()], "привет".as_bytes())
            .await
            .expect("encrypt");
        assert_eq!(encrypted.len(), 1);
        assert_eq!(encrypted[0].olm_type, OlmMessageType::Prekey);
        assert_eq!(encrypted[0].e2e_version, rchat_proto::E2E_VERSION_OLM_V1);
        assert_ne!(encrypted[0].ciphertext, b"privet");

        let plaintext = alice
            .decrypt_from(&bob_peer, &encrypted[0])
            .await
            .expect("decrypt");
        assert_eq!(plaintext, "привет".as_bytes());

        // Alice отвечает: сессия уже есть — обычное сообщение.
        let reply = alice
            .encrypt_for(std::slice::from_ref(&bob_peer), "ответ".as_bytes())
            .await
            .expect("encrypt reply");
        assert_eq!(reply[0].olm_type, OlmMessageType::Normal);
        let plaintext = bob
            .decrypt_from(&alice_peer, &reply[0])
            .await
            .expect("decrypt reply");
        assert_eq!(plaintext, "ответ".as_bytes());
    }

    #[tokio::test]
    async fn state_survives_restart() {
        let store = Arc::new(MemStore::default());
        let secrets = Arc::new(MemSecrets::default());
        let alice = provider_with(store.clone(), secrets.clone());
        let bob = provider();

        let (mut alice_peer, alice_keys) = peer(&alice, "a1").await;
        let (bob_peer, _) = peer(&bob, "b1").await;
        alice_peer.one_time_key = Some(alice_keys.one_time_keys[0].clone());
        let encrypted = bob
            .encrypt_for(&[alice_peer.clone()], "до перезапуска".as_bytes())
            .await
            .expect("encrypt");
        alice
            .decrypt_from(&bob_peer, &encrypted[0])
            .await
            .expect("decrypt");

        // Новый экземпляр провайдера с теми же хранилищами.
        let alice_restarted = provider_with(store, secrets);
        alice_restarted.initialize().await.expect("init");
        let (ed25519, curve25519) = alice_restarted.identity_keys().await.expect("identity");
        assert_eq!(ed25519, alice_peer.ed25519);
        assert_eq!(curve25519, alice_peer.curve25519);
        assert!(alice_restarted.has_session("b1").await.expect("session"));

        // Расшифровка следующего сообщения после «перезапуска».
        let next = bob
            .encrypt_for(&[alice_peer.clone()], "после перезапуска".as_bytes())
            .await
            .expect("encrypt");
        // Olm повторяет prekey-сообщение, пока отправитель не получит ответ:
        // второй сообщение тоже prekey, и ключ уже израсходован — провайдер
        // обязан расшифровать его установленной сессией.
        assert_eq!(next[0].olm_type, OlmMessageType::Prekey);
        let plaintext = alice_restarted
            .decrypt_from(&bob_peer, &next[0])
            .await
            .expect("decrypt");
        assert_eq!(plaintext, "после перезапуска".as_bytes());

        // После ответа Alice сессия завершена: Bob шлёт обычные сообщения.
        let ack = alice_restarted
            .encrypt_for(std::slice::from_ref(&bob_peer), "ответ".as_bytes())
            .await
            .expect("encrypt ack");
        assert_eq!(ack[0].olm_type, OlmMessageType::Normal);
        let ack_plain = bob
            .decrypt_from(&alice_peer, &ack[0])
            .await
            .expect("decrypt ack");
        assert_eq!(ack_plain, "ответ".as_bytes());
        let normal = bob
            .encrypt_for(&[alice_peer.clone()], "теперь обычное".as_bytes())
            .await
            .expect("encrypt normal");
        assert_eq!(normal[0].olm_type, OlmMessageType::Normal);
        let plaintext = alice_restarted
            .decrypt_from(&bob_peer, &normal[0])
            .await
            .expect("decrypt normal");
        assert_eq!(plaintext, "теперь обычное".as_bytes());
    }

    #[tokio::test]
    async fn uploaded_key_ids_do_not_collide_between_otk_and_fallback() {
        let provider = provider();
        provider.initialize().await.expect("init");
        let upload = provider.keys_for_upload(10).await.expect("keys");
        assert_eq!(upload.one_time_keys.len(), 10);
        let fallback = upload.fallback_key.expect("fallback");
        assert!(fallback.fallback);
        assert_ne!(
            fallback.key_id & FALLBACK_KEY_ID_BIT,
            0,
            "резервный ключ должен быть в отдельном пространстве id"
        );
        for key in &upload.one_time_keys {
            assert!(!key.fallback);
            assert_ne!(key.key_id, fallback.key_id);
            assert_eq!(key.key_id & FALLBACK_KEY_ID_BIT, 0);
        }

        // Повторный запрос без публикации возвращает те же ключи: сервер
        // должен принимать такую повторную загрузку идемпотентно.
        let again = provider.keys_for_upload(10).await.expect("keys again");
        assert_eq!(again.one_time_keys.len(), 10);
        assert_eq!(
            again.fallback_key.expect("fallback").key_id,
            fallback.key_id
        );
    }

    #[tokio::test]
    async fn missing_prekey_is_an_error_not_plaintext() {
        let bob = provider();
        let (alice_peer, _) = peer(&provider(), "a1").await;
        let (_bob_peer, _) = peer(&bob, "b1").await;

        let error = bob
            .encrypt_for(&[alice_peer], "секрет".as_bytes())
            .await
            .expect_err("должна быть ошибка");
        assert!(matches!(error, CryptoError::MissingPreKey(id) if id == "a1"));
    }

    #[tokio::test]
    async fn wrong_identity_key_is_rejected() {
        let alice = provider();
        let bob = provider();
        let (bob_peer, _) = peer(&bob, "b1").await;
        let (mut alice_peer, alice_keys) = peer(&alice, "a1").await;

        alice_peer.one_time_key = Some(alice_keys.one_time_keys[0].clone());
        let encrypted = bob
            .encrypt_for(&[alice_peer.clone()], "привет".as_bytes())
            .await
            .expect("encrypt");

        // Подмена identity key отправителя должна привести к отказу.
        let mut forged = bob_peer.clone();
        forged.curve25519 = alice_peer.curve25519.clone();
        let error = alice
            .decrypt_from(&forged, &encrypted[0])
            .await
            .expect_err("подмена ключа должна отвергаться");
        assert!(matches!(error, CryptoError::IdentityMismatch(id) if id == "b1"));
    }

    #[tokio::test]
    async fn corrupted_ciphertext_is_rejected() {
        let alice = provider();
        let bob = provider();
        let (bob_peer, _) = peer(&bob, "b1").await;
        let (mut alice_peer, alice_keys) = peer(&alice, "a1").await;
        alice_peer.one_time_key = Some(alice_keys.one_time_keys[0].clone());
        let mut encrypted = bob
            .encrypt_for(&[alice_peer.clone()], "привет".as_bytes())
            .await
            .expect("encrypt");
        encrypted[0].ciphertext[10] ^= 0xFF;

        let error = alice
            .decrypt_from(&bob_peer, &encrypted[0])
            .await
            .expect_err("испорченный ciphertext должен отвергаться");
        assert!(matches!(
            error,
            CryptoError::DecryptFailed(_, _) | CryptoError::Session(_, _)
        ));
    }

    #[tokio::test]
    async fn wipe_removes_state_and_rotates_identity() {
        let store = Arc::new(MemStore::default());
        let secrets = Arc::new(MemSecrets::default());
        let provider = provider_with(store.clone(), secrets.clone());
        let (peer_before, keys) = peer(&provider, "a1").await;
        assert!(!keys.one_time_keys.is_empty());

        provider.wipe().await.expect("wipe");
        assert!(store.values.lock().expect("poisoned").is_empty());
        assert!(secrets.values.lock().expect("poisoned").is_empty());

        // После очистки создаётся новый аккаунт с новой идентичностью.
        provider.initialize().await.expect("init");
        let (ed25519, curve25519) = provider.identity_keys().await.expect("identity");
        assert_ne!(ed25519, peer_before.ed25519);
        assert_ne!(curve25519, peer_before.curve25519);
        assert!(!provider.has_session("b1").await.expect("session"));
    }

    // --- Megolm: групповые E2E-сессии ---

    /// Полный цикл: общий megolm-ciphertext + доставка ключа Olm-конвертом.
    #[tokio::test]
    async fn group_round_trip_with_key_share() {
        let alice = provider();
        let bob = provider();
        let (alice_peer, _) = peer(&alice, "a1").await;
        let (mut bob_peer, bob_keys) = peer(&bob, "b1").await;

        let encryption = alice
            .group_encrypt("c1", "привет группе".as_bytes(), true)
            .await
            .expect("group encrypt");
        let session_key = encryption.session_key.clone().expect("ключ для рассылки");

        // Доставка ключа: Olm-конверт для устройства bob.
        bob_peer.one_time_key = Some(bob_keys.one_time_keys[0].clone());
        let envelopes = alice
            .encrypt_group_key("c1", &session_key, std::slice::from_ref(&bob_peer))
            .await
            .expect("encrypt key");
        assert_eq!(envelopes.len(), 1);
        assert_eq!(envelopes[0].olm_type, OlmMessageType::Prekey);

        let info = bob
            .apply_group_key(&alice_peer, &envelopes[0], "c1")
            .await
            .expect("import key");
        assert_eq!(info.chat_id, "c1");
        assert_eq!(info.session_id, encryption.session_id);

        let plaintext = bob
            .decrypt_group("c1", &info.session_id, &encryption.ciphertext)
            .await
            .expect("decrypt group");
        assert_eq!(plaintext, "привет группе".as_bytes());

        // Повторная доставка того же ключа (новым Olm-сообщением) безопасна:
        // сессия уже есть, ratchet не откатывается. Пока bob не ответил,
        // olm-сессия остаётся в «prekey»-состоянии — при повторной доставке
        // prekey-сообщения ключ израсходован, и провайдер идёт по ветке
        // существующей сессии.
        let second_envelope = alice
            .encrypt_group_key("c1", &session_key, std::slice::from_ref(&bob_peer))
            .await
            .expect("encrypt key again");
        let again = bob
            .apply_group_key(&alice_peer, &second_envelope[0], "c1")
            .await
            .expect("re-import");
        assert_eq!(again.session_id, encryption.session_id);
    }

    #[tokio::test]
    async fn group_key_is_bound_to_its_chat() {
        let alice = provider();
        let bob = provider();
        let (alice_peer, _) = peer(&alice, "a1").await;
        let (mut bob_peer, bob_keys) = peer(&bob, "b1").await;
        bob_peer.one_time_key = Some(bob_keys.one_time_keys[0].clone());

        let encryption = alice
            .group_encrypt("c1", b"x", true)
            .await
            .expect("encrypt");
        let key = encryption.session_key.clone().expect("key");
        let envelopes = alice
            .encrypt_group_key("c1", &key, std::slice::from_ref(&bob_peer))
            .await
            .expect("envelope");

        // Ключ отклоняется для другого чата и не импортируется.
        let error = bob
            .apply_group_key(&alice_peer, &envelopes[0], "c2")
            .await
            .expect_err("чужой чат");
        assert!(matches!(error, CryptoError::DecryptFailed(_, _)));
        let error = bob
            .decrypt_group("c1", &encryption.session_id, &encryption.ciphertext)
            .await
            .expect_err("сессия не должна импортироваться");
        assert!(matches!(error, CryptoError::DecryptFailed(_, _)));

        // Корректный импорт отдельным конвертом.
        let envelopes = alice
            .encrypt_group_key("c1", &key, std::slice::from_ref(&bob_peer))
            .await
            .expect("envelope retry");
        bob.apply_group_key(&alice_peer, &envelopes[0], "c1")
            .await
            .expect("import");

        // Сессией чата c1 нельзя расшифровать сообщение «в чате» c2.
        let error = bob
            .decrypt_group("c2", &encryption.session_id, &encryption.ciphertext)
            .await
            .expect_err("другой чат");
        assert!(matches!(error, CryptoError::DecryptFailed(_, _)));
    }

    #[tokio::test]
    async fn group_key_shared_later_does_not_decrypt_earlier_messages() {
        let alice = provider();
        let bob = provider();
        let (alice_peer, _) = peer(&alice, "a1").await;
        let (mut bob_peer, bob_keys) = peer(&bob, "b1").await;
        bob_peer.one_time_key = Some(bob_keys.one_time_keys[0].clone());

        let first = alice.group_encrypt("c1", b"m1", false).await.expect("m1");
        assert!(first.session_key.is_none());
        let second = alice.group_encrypt("c1", b"m2", true).await.expect("m2");
        assert_eq!(first.session_id, second.session_id);
        let key = second.session_key.clone().expect("key");

        let envelopes = alice
            .encrypt_group_key("c1", &key, std::slice::from_ref(&bob_peer))
            .await
            .expect("envelope");
        let info = bob
            .apply_group_key(&alice_peer, &envelopes[0], "c1")
            .await
            .expect("import");

        // Ключ выдан с индекса второго сообщения: первое недоступно.
        let error = bob
            .decrypt_group("c1", &info.session_id, &first.ciphertext)
            .await
            .expect_err("раннее сообщение");
        assert!(matches!(error, CryptoError::DecryptFailed(_, _)));
        let plaintext = bob
            .decrypt_group("c1", &info.session_id, &second.ciphertext)
            .await
            .expect("m2");
        assert_eq!(plaintext, b"m2".to_vec());
    }

    #[tokio::test]
    async fn group_rotation_creates_new_session_and_keeps_history() {
        let alice = provider();
        let bob = provider();
        let (alice_peer, _) = peer(&alice, "a1").await;
        let (mut bob_peer, bob_keys) = peer(&bob, "b1").await;
        bob_peer.one_time_key = Some(bob_keys.one_time_keys[0].clone());

        let before = alice
            .group_encrypt("c1", "до ротации".as_bytes(), true)
            .await
            .expect("before");
        let key = before.session_key.clone().expect("key");
        let envelopes = alice
            .encrypt_group_key("c1", &key, std::slice::from_ref(&bob_peer))
            .await
            .expect("envelope");
        let info_before = bob
            .apply_group_key(&alice_peer, &envelopes[0], "c1")
            .await
            .expect("import before");

        // Ротация: следующая исходящая сессия другая.
        alice.rotate_group_session("c1").await.expect("rotate");
        let after = alice
            .group_encrypt("c1", "после ротации".as_bytes(), true)
            .await
            .expect("after");
        assert_ne!(after.session_id, before.session_id);
        let key = after.session_key.clone().expect("key");
        let envelopes = alice
            .encrypt_group_key("c1", &key, std::slice::from_ref(&bob_peer))
            .await
            .expect("envelope after");
        let info_after = bob
            .apply_group_key(&alice_peer, &envelopes[0], "c1")
            .await
            .expect("import after");
        assert_eq!(info_after.session_id, after.session_id);

        // История остаётся читаемой: старая сессия не удалена у получателя.
        let plaintext = bob
            .decrypt_group("c1", &info_before.session_id, &before.ciphertext)
            .await
            .expect("old");
        assert_eq!(plaintext, "до ротации".as_bytes());
        let plaintext = bob
            .decrypt_group("c1", &info_after.session_id, &after.ciphertext)
            .await
            .expect("new");
        assert_eq!(plaintext, "после ротации".as_bytes());
        // Новой сессией старое сообщение не расшифровывается.
        assert!(bob
            .decrypt_group("c1", &info_after.session_id, &before.ciphertext)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn group_state_survives_restart() {
        let alice_store = Arc::new(MemStore::default());
        let alice_secrets = Arc::new(MemSecrets::default());
        let bob_store = Arc::new(MemStore::default());
        let bob_secrets = Arc::new(MemSecrets::default());
        let alice = provider_with(alice_store.clone(), alice_secrets.clone());
        let bob = provider_with(bob_store.clone(), bob_secrets.clone());
        let (alice_peer, _) = peer(&alice, "a1").await;
        let (mut bob_peer, bob_keys) = peer(&bob, "b1").await;
        bob_peer.one_time_key = Some(bob_keys.one_time_keys[0].clone());

        let first = alice.group_encrypt("c1", b"m1", true).await.expect("m1");
        let key = first.session_key.clone().expect("key");
        let envelopes = alice
            .encrypt_group_key("c1", &key, std::slice::from_ref(&bob_peer))
            .await
            .expect("envelope");
        let info = bob
            .apply_group_key(&alice_peer, &envelopes[0], "c1")
            .await
            .expect("import");
        assert_eq!(
            bob.decrypt_group("c1", &info.session_id, &first.ciphertext)
                .await
                .expect("m1"),
            b"m1".to_vec()
        );

        // «Перезапуск» обеих сторон: те же хранилища, новые экземпляры.
        let alice = provider_with(alice_store, alice_secrets);
        let bob = provider_with(bob_store, bob_secrets);
        let second = alice.group_encrypt("c1", b"m2", true).await.expect("m2");
        // Исходящая сессия сохранилась: тот же идентификатор.
        assert_eq!(second.session_id, first.session_id);
        // Входящая сессия bob тоже сохранилась.
        assert_eq!(
            bob.decrypt_group("c1", &info.session_id, &second.ciphertext)
                .await
                .expect("m2 после перезапуска"),
            b"m2".to_vec()
        );
    }

    #[tokio::test]
    async fn group_wipe_removes_group_state() {
        let alice_store = Arc::new(MemStore::default());
        let alice_secrets = Arc::new(MemSecrets::default());
        let alice = provider_with(alice_store.clone(), alice_secrets.clone());
        let (mut alice_peer, alice_keys) = peer(&alice, "a1").await;
        let bob = provider();
        let (bob_peer, _) = peer(&bob, "b1").await;

        // У alice: исходящая сессия c1 и входящая (ключ от bob для c2).
        let outgoing = alice.group_encrypt("c1", b"m1", true).await.expect("out");
        alice_peer.one_time_key = Some(alice_keys.one_time_keys[0].clone());
        let bob_encryption = bob.group_encrypt("c2", b"m2", true).await.expect("bob out");
        let key = bob_encryption.session_key.clone().expect("key");
        let envelopes = bob
            .encrypt_group_key("c2", &key, std::slice::from_ref(&alice_peer))
            .await
            .expect("envelope");
        alice
            .apply_group_key(&bob_peer, &envelopes[0], "c2")
            .await
            .expect("import");
        {
            let values = alice_store.values.lock().expect("poisoned");
            assert!(values.keys().any(|k| k.starts_with("megolm_out:")));
            assert!(values.keys().any(|k| k.starts_with("megolm_in:")));
        }

        alice.wipe().await.expect("wipe");
        assert!(!alice_store
            .values
            .lock()
            .expect("poisoned")
            .keys()
            .any(|k| k.starts_with("megolm")));

        // После очистки создаётся новая исходящая сессия.
        let fresh = alice
            .group_encrypt("c1", b"m3", false)
            .await
            .expect("fresh");
        assert_ne!(fresh.session_id, outgoing.session_id);
    }

    #[tokio::test]
    async fn group_key_payload_rejects_malformed_input() {
        let provider = provider();
        let encryption = provider
            .group_encrypt("c1", b"x", true)
            .await
            .expect("encrypt");
        let key = encryption.session_key.expect("key");
        let payload = build_group_key_payload("c1", &key).expect("build");
        assert_eq!(parse_group_key_payload(&payload).expect("parse").0, "c1");

        assert!(parse_group_key_payload(&[]).is_err());
        assert!(parse_group_key_payload(&payload[..payload.len() - 1]).is_err());
        let mut bad_magic = payload.clone();
        bad_magic[0] = b'X';
        assert!(parse_group_key_payload(&bad_magic).is_err());
        let mut bad_version = payload.clone();
        bad_version[4] = 99;
        assert!(parse_group_key_payload(&bad_version).is_err());
        let mut bad_key = payload.clone();
        let last = bad_key.len() - 1;
        bad_key[last] ^= 0xFF;
        assert!(parse_group_key_payload(&bad_key).is_err());
        // Пустой chat_id — невалидный payload.
        let empty_chat = build_group_key_payload("", &key).expect("build");
        assert!(parse_group_key_payload(&empty_chat).is_err());
    }
}

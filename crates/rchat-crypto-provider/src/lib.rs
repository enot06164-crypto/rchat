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
    secret_keys, CryptoProvider, CryptoStateStore, EncryptedMessage, KeyUpload, OneTimeKey,
    PeerDevice, SecretStore,
};
use rchat_proto::proto::OlmMessageType;
use tokio::sync::{Mutex, MutexGuard};
use vodozemac::olm::{
    Account, AccountPickle, OlmMessage, Session, SessionConfig, SessionCreationError, SessionPickle,
};
use vodozemac::{base64_decode, Curve25519PublicKey, KeyId};
use zeroize::Zeroizing;

/// Ключ аккаунта в [`CryptoStateStore`].
const ACCOUNT_STATE_KEY: &str = "olm_account";
/// Префикс ключей сессий: `olm_session:<device_id>`.
const SESSION_STATE_PREFIX: &str = "olm_session:";
/// Длина ключа шифрования pickle (требование vodozemac).
const PICKLE_KEY_LEN: usize = 32;

/// E2E-провайдер Olm.
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
}

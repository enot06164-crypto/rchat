//! Резервное хранилище секретов: шифрованный файл и отдельный ключевой файл.
//!
//! Платформенное защищённое хранилище (Secret Service/Keychain) не входит в
//! зависимости первого релиза, поэтому применяется документированный
//! резервный способ:
//!
//! * все секреты лежат в одном файле данных, зашифрованном
//!   XChaCha20-Poly1305 (AEAD, случайный 24-байтный nonce на каждую запись);
//! * ключ шифрования (32 случайных байта) хранится **отдельно**, в файле с
//!   правами `0600`; каталог создаётся с правами `0700`;
//! * ключ и значения не выводятся в `Debug` и не попадают в логи;
//! * запись атомарная: временный файл → `fsync` → `rename`, поэтому сбой не
//!   оставляет частично записанный секрет.
//!
//! Формат файла данных: `nonce (24) || ciphertext`, где внутри шифротекста —
//! заголовок `RCHS<version>` и список пар «имя → значение» с ограничениями по
//! размеру, чтобы повреждённый или подменённый файл не приводил к
//! неограниченному выделению памяти.
//!
//! Ограничения (важно для безопасности):
//!
//! * защита опирается на права доступа файловой системы: при доступе
//!   злоумышленника к каталогу пользователя секреты скомпрометированы
//!   (ключ лежит рядом с шифротекстом и защищён только правами `0600`);
//! * хранилище однопроцессное: конкурентная запись из двух процессов не
//!   синхронизируется (файл перезаписывается целиком, последняя запись
//!   побеждает);
//! * на не-Unix платформах права `0600`/`0700` не выставляются, полагаться на
//!   ограничение доступа там нельзя.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rchat_core::error::SecretStoreError;
use zeroize::Zeroizing;

/// Длина ключа шифрования (XChaCha20-Poly1305).
const KEY_LEN: usize = 32;
/// Длина nonce (XChaCha20).
const NONCE_LEN: usize = 24;
/// Максимальный размер файла данных.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Максимальное число записей.
const MAX_ENTRIES: u32 = 4096;
/// Максимальная длина имени секрета.
const MAX_NAME_BYTES: u32 = 256;
/// Максимальная длина значения.
const MAX_VALUE_BYTES: u32 = 256 * 1024;
/// Заголовок открытого текста (магия + версия формата).
const MAGIC: &[u8] = b"RCHS\x01";

/// Хранилище секретов в шифрованном файле.
pub struct EncryptedFileSecretStore {
    data_path: PathBuf,
    key_path: PathBuf,
    key: Zeroizing<[u8; KEY_LEN]>,
    entries: Mutex<BTreeMap<String, Zeroizing<Vec<u8>>>>,
}

impl std::fmt::Debug for EncryptedFileSecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptedFileSecretStore")
            .field("data_path", &self.data_path)
            .field("key_path", &self.key_path)
            .field("key", &"<redacted>")
            .field(
                "entries",
                &self.entries.lock().map(|map| map.len()).unwrap_or(0),
            )
            .finish()
    }
}

impl EncryptedFileSecretStore {
    /// Открывает (создаёт при необходимости) хранилище.
    ///
    /// `data_path` — шифрованный файл секретов, `key_path` — файл ключа
    /// шифрования. Родительские каталоги создаются с правами `0700`.
    pub fn open(
        data_path: impl AsRef<Path>,
        key_path: impl AsRef<Path>,
    ) -> Result<Self, SecretStoreError> {
        let data_path = data_path.as_ref().to_path_buf();
        let key_path = key_path.as_ref().to_path_buf();
        for path in [&data_path, &key_path] {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    ensure_private_dir(parent)?;
                }
            }
        }
        if data_path == key_path {
            return Err(SecretStoreError::Unavailable(
                "файл ключа и файл секретов должны различаться".to_string(),
            ));
        }
        let key = load_or_create_key(&key_path)?;
        let entries = load_entries(&data_path, &key)?;
        Ok(Self {
            data_path,
            key_path,
            key,
            entries: Mutex::new(entries),
        })
    }

    /// Путь к шифрованному файлу секретов.
    pub fn data_path(&self) -> &Path {
        &self.data_path
    }

    fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, SecretStoreError> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce_bytes).map_err(|e| {
            SecretStoreError::Unavailable(format!("нет источника случайности: {e}"))
        })?;
        let cipher = XChaCha20Poly1305::new_from_slice(self.key.as_ref())
            .map_err(|e| SecretStoreError::Unavailable(format!("ключ шифрования: {e}")))?;
        let nonce = XNonce::from(nonce_bytes);
        let mut ciphertext = cipher
            .encrypt(&nonce, plaintext)
            .map_err(|e| SecretStoreError::Unavailable(format!("шифрование секретов: {e}")))?;
        let mut file = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        file.extend_from_slice(&nonce_bytes);
        file.append(&mut ciphertext);
        Ok(file)
    }

    fn write(
        &self,
        entries: &BTreeMap<String, Zeroizing<Vec<u8>>>,
    ) -> Result<(), SecretStoreError> {
        let plaintext = serialize(entries)?;
        let file_bytes = self.encrypt(plaintext.as_ref())?;
        write_atomic(&self.data_path, &file_bytes)
    }

    fn mutate(
        &self,
        apply: impl FnOnce(&mut BTreeMap<String, Zeroizing<Vec<u8>>>),
    ) -> Result<(), SecretStoreError> {
        let mut guard = self
            .entries
            .lock()
            .map_err(|_| SecretStoreError::Unavailable("secret store poisoned".to_string()))?;
        let mut next = guard.clone();
        apply(&mut next);
        self.write(&next)?;
        *guard = next;
        Ok(())
    }
}

impl rchat_core::SecretStore for EncryptedFileSecretStore {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        let guard = self
            .entries
            .lock()
            .map_err(|_| SecretStoreError::Unavailable("secret store poisoned".to_string()))?;
        Ok(guard.get(key).map(|value| value.to_vec()))
    }

    fn set(&self, key: &str, value: &[u8]) -> Result<(), SecretStoreError> {
        if key.is_empty() {
            return Err(SecretStoreError::Unavailable(
                "пустое имя секрета".to_string(),
            ));
        }
        let value = value.to_vec();
        self.mutate(|entries| {
            entries.insert(key.to_string(), Zeroizing::new(value));
        })
    }

    fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
        self.mutate(|entries| {
            entries.remove(key);
        })
    }
}

/// Загружает ключ шифрования или создаёт новый (ровно 32 случайных байта).
fn load_or_create_key(path: &Path) -> Result<Zeroizing<[u8; KEY_LEN]>, SecretStoreError> {
    match fs::read(path) {
        Ok(bytes) => {
            if bytes.len() != KEY_LEN {
                return Err(SecretStoreError::Corrupted(format!(
                    "файл ключа {} повреждён: ожидалось {KEY_LEN} байт, получено {}",
                    path.display(),
                    bytes.len()
                )));
            }
            let mut key = [0u8; KEY_LEN];
            key.copy_from_slice(&bytes);
            enforce_private_file(path);
            Ok(Zeroizing::new(key))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut key = [0u8; KEY_LEN];
            getrandom::fill(&mut key).map_err(|e| {
                SecretStoreError::Unavailable(format!("нет источника случайности: {e}"))
            })?;
            write_atomic(path, &key)?;
            Ok(Zeroizing::new(key))
        }
        Err(error) => Err(SecretStoreError::Io(format!(
            "чтение ключа {}: {error}",
            path.display()
        ))),
    }
}

fn load_entries(
    path: &Path,
    key: &[u8; KEY_LEN],
) -> Result<BTreeMap<String, Zeroizing<Vec<u8>>>, SecretStoreError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BTreeMap::new());
        }
        Err(error) => {
            return Err(SecretStoreError::Io(format!(
                "метаданные {}: {error}",
                path.display()
            )))
        }
    };
    if metadata.len() > MAX_FILE_BYTES {
        return Err(SecretStoreError::Corrupted(format!(
            "файл секретов {} превышает лимит {MAX_FILE_BYTES} байт",
            path.display()
        )));
    }
    let bytes = fs::read(path).map_err(|error| {
        SecretStoreError::Io(format!("чтение секретов {}: {error}", path.display()))
    })?;
    if bytes.len() < NONCE_LEN + 16 {
        return Err(SecretStoreError::Corrupted(format!(
            "файл секретов {} короче минимального размера",
            path.display()
        )));
    }
    let (nonce_bytes, ciphertext) = bytes.split_at(NONCE_LEN);
    let nonce = XNonce::from(
        <[u8; NONCE_LEN]>::try_from(nonce_bytes)
            .map_err(|_| SecretStoreError::Corrupted("некорректный размер nonce".to_string()))?,
    );
    let cipher = XChaCha20Poly1305::new_from_slice(key)
        .map_err(|e| SecretStoreError::Unavailable(format!("ключ шифрования: {e}")))?;
    let plaintext = cipher.decrypt(&nonce, ciphertext).map_err(|_| {
        SecretStoreError::Corrupted(format!(
            "не удалось расшифровать {} (не тот ключ или файл повреждён)",
            path.display()
        ))
    })?;
    deserialize(path, &plaintext)
}

/// Формат открытого текста: `MAGIC || u32 count || (u32 name_len, name, u32 value_len, value)*`.
fn serialize(entries: &BTreeMap<String, Zeroizing<Vec<u8>>>) -> Result<Vec<u8>, SecretStoreError> {
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(MAGIC);
    let count = u32::try_from(entries.len())
        .map_err(|_| SecretStoreError::Unavailable("слишком много секретов".to_string()))?;
    if count > MAX_ENTRIES {
        return Err(SecretStoreError::Unavailable(format!(
            "слишком много секретов: {count} (максимум {MAX_ENTRIES})"
        )));
    }
    out.extend_from_slice(&count.to_le_bytes());
    for (name, value) in entries {
        let name_bytes = name.as_bytes();
        if name_bytes.len() > MAX_NAME_BYTES as usize {
            return Err(SecretStoreError::Unavailable(format!(
                "имя секрета длиннее {MAX_NAME_BYTES} байт"
            )));
        }
        if value.len() > MAX_VALUE_BYTES as usize {
            return Err(SecretStoreError::Unavailable(format!(
                "значение секрета длиннее {MAX_VALUE_BYTES} байт"
            )));
        }
        out.extend_from_slice(&(name_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(&(value.len() as u32).to_le_bytes());
        out.extend_from_slice(value);
    }
    Ok(out)
}

fn deserialize(
    path: &Path,
    plaintext: &[u8],
) -> Result<BTreeMap<String, Zeroizing<Vec<u8>>>, SecretStoreError> {
    let corrupted = |detail: &str| {
        SecretStoreError::Corrupted(format!("файл секретов {}: {detail}", path.display()))
    };
    let mut cursor = plaintext;
    if cursor.len() < MAGIC.len() || &cursor[..MAGIC.len()] != MAGIC {
        return Err(corrupted("неизвестный формат (нет заголовка RCHS)"));
    }
    cursor = &cursor[MAGIC.len()..];
    let count = take_u32(&mut cursor).ok_or_else(|| corrupted("обрыв на числе записей"))?;
    if count > MAX_ENTRIES {
        return Err(corrupted("слишком много записей"));
    }
    let mut entries = BTreeMap::new();
    for _ in 0..count {
        let name_len = take_u32(&mut cursor).ok_or_else(|| corrupted("обрыв на длине имени"))?;
        if name_len == 0 || name_len > MAX_NAME_BYTES {
            return Err(corrupted("некорректная длина имени"));
        }
        let name_bytes =
            take_bytes(&mut cursor, name_len as usize).ok_or_else(|| corrupted("обрыв имени"))?;
        let name = std::str::from_utf8(name_bytes)
            .map_err(|_| corrupted("имя секрета не является UTF-8"))?
            .to_string();
        let value_len =
            take_u32(&mut cursor).ok_or_else(|| corrupted("обрыв на длине значения"))?;
        if value_len > MAX_VALUE_BYTES {
            return Err(corrupted("значение секрета длиннее допустимого"));
        }
        let value = take_bytes(&mut cursor, value_len as usize)
            .ok_or_else(|| corrupted("обрыв значения"))?;
        entries.insert(name, Zeroizing::new(value.to_vec()));
    }
    if !cursor.is_empty() {
        return Err(corrupted("лишние данные после записей"));
    }
    Ok(entries)
}

fn take_u32(cursor: &mut &[u8]) -> Option<u32> {
    let bytes = take_bytes(cursor, 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn take_bytes<'a>(cursor: &mut &'a [u8], len: usize) -> Option<&'a [u8]> {
    if cursor.len() < len {
        return None;
    }
    let (head, tail) = cursor.split_at(len);
    *cursor = tail;
    Some(head)
}

/// Атомарная запись: временный файл в том же каталоге, `fsync`, переименование.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), SecretStoreError> {
    let tmp_path = match path.file_name() {
        Some(name) => {
            let mut tmp = name.to_os_string();
            tmp.push(".tmp");
            path.with_file_name(tmp)
        }
        None => {
            return Err(SecretStoreError::Io(format!(
                "некорректный путь {}",
                path.display()
            )))
        }
    };
    {
        let mut file = open_private(&tmp_path)?;
        file.write_all(bytes)
            .map_err(|e| SecretStoreError::Io(format!("запись {}: {e}", tmp_path.display())))?;
        file.sync_all()
            .map_err(|e| SecretStoreError::Io(format!("fsync {}: {e}", tmp_path.display())))?;
    }
    fs::rename(&tmp_path, path).map_err(|e| {
        SecretStoreError::Io(format!(
            "переименование {} → {}: {e}",
            tmp_path.display(),
            path.display()
        ))
    })?;
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

fn open_private(path: &Path) -> Result<File, SecretStoreError> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|e| SecretStoreError::Io(format!("открытие {}: {e}", path.display())))
}

fn ensure_private_dir(dir: &Path) -> Result<(), SecretStoreError> {
    fs::create_dir_all(dir)
        .map_err(|e| SecretStoreError::Io(format!("каталог {}: {e}", dir.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|e| SecretStoreError::Io(format!("права на {}: {e}", dir.display())))?;
    }
    Ok(())
}

/// Понижает права уже существующего файла до `0600` (best-effort).
fn enforce_private_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

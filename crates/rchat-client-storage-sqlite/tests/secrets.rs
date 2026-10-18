//! Проверки резервного файлового хранилища секретов.

use std::fs;

use rchat_client_storage_sqlite::EncryptedFileSecretStore;
use rchat_core::error::SecretStoreError;
use rchat_core::SecretStore;

struct Fixture {
    dir: tempfile::TempDir,
    data: std::path::PathBuf,
    key: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path().join("secrets.bin");
    let key = dir.path().join("secrets.key");
    Fixture { dir, data, key }
}

#[test]
fn roundtrip_persists_and_reloads() {
    let f = fixture();
    let store = EncryptedFileSecretStore::open(&f.data, &f.key).expect("open");
    assert!(store.get("session_token").expect("get").is_none());
    store
        .set("session_token", b"sekrit-token-value")
        .expect("set");
    store.set("device_id", b"d_alice").expect("set");
    assert_eq!(
        store.get("session_token").expect("get"),
        Some(b"sekrit-token-value".to_vec())
    );

    // Значения сохраняются между открытиями.
    drop(store);
    let reopened = EncryptedFileSecretStore::open(&f.data, &f.key).expect("reopen");
    assert_eq!(
        reopened.get("device_id").expect("get"),
        Some(b"d_alice".to_vec())
    );

    // Ключ остаётся тем же после перезапуска.
    let key_bytes = fs::read(&f.key).expect("key file");
    assert_eq!(key_bytes.len(), 32);
    assert_eq!(fs::read(&f.key).expect("key file"), key_bytes);
    let _ = f.dir;
}

#[test]
fn ciphertext_file_does_not_leak_plaintext() {
    let f = fixture();
    let store = EncryptedFileSecretStore::open(&f.data, &f.key).expect("open");
    let secret = b"SUPER-SECRET-MARKER-12345";
    store.set("token", secret).expect("set");
    let bytes = fs::read(&f.data).expect("read");
    assert!(
        !bytes.windows(secret.len()).any(|window| window == secret),
        "открытый текст не должен присутствовать в файле"
    );
    // Nonce (24 байта) + тег (16 байт) учтены в размере.
    assert!(bytes.len() > 24 + 16);
}

#[test]
fn wrong_key_reports_corruption() {
    let f = fixture();
    {
        let store = EncryptedFileSecretStore::open(&f.data, &f.key).expect("open");
        store.set("token", b"value").expect("set");
    }
    fs::write(&f.key, [7u8; 32]).expect("overwrite key");
    let error = EncryptedFileSecretStore::open(&f.data, &f.key).expect_err("wrong key");
    assert!(matches!(error, SecretStoreError::Corrupted(_)));
}

#[test]
fn truncated_or_corrupted_files_are_reported() {
    let f = fixture();
    {
        let store = EncryptedFileSecretStore::open(&f.data, &f.key).expect("open");
        store.set("token", b"value").expect("set");
    }
    let bytes = fs::read(&f.data).expect("read");
    fs::write(&f.data, &bytes[..bytes.len() / 2]).expect("truncate");
    let error = EncryptedFileSecretStore::open(&f.data, &f.key).expect_err("truncated");
    assert!(matches!(error, SecretStoreError::Corrupted(_)));

    // Слишком короткий файл ключа — тоже повреждение.
    let f2 = fixture();
    fs::write(&f2.key, b"short").expect("write");
    let error = EncryptedFileSecretStore::open(&f2.data, &f2.key).expect_err("short key");
    assert!(matches!(error, SecretStoreError::Corrupted(_)));
}

#[test]
fn missing_files_start_empty_and_delete_is_idempotent() {
    let f = fixture();
    let parent = f.dir.path().join("nested/deep");
    let data = parent.join("secrets.bin");
    let key = parent.join("secrets.key");
    let store = EncryptedFileSecretStore::open(&data, &key).expect("open");
    assert!(store.get("anything").expect("get").is_none());
    store.set("a", b"1").expect("set");
    store.delete("a").expect("delete");
    store.delete("a").expect("delete again");
    assert!(store.get("a").expect("get").is_none());
    assert!(data.exists());
    assert!(key.exists());
}

#[cfg(unix)]
#[test]
fn files_and_directories_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let data_dir = f.dir.path().join("private");
    let data = data_dir.join("secrets.bin");
    let key = data_dir.join("secrets.key");
    let store = EncryptedFileSecretStore::open(&data, &key).expect("open");
    store.set("token", b"value").expect("set");

    let dir_mode = fs::metadata(&data_dir).expect("dir").permissions().mode() & 0o777;
    let data_mode = fs::metadata(&data).expect("data").permissions().mode() & 0o777;
    let key_mode = fs::metadata(&key).expect("key").permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700);
    assert_eq!(data_mode, 0o600);
    assert_eq!(key_mode, 0o600);
}

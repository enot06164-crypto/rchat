//! Хеширование паролей — отдельная ответственность сервера (не E2E-крипто).
//!
//! Интерфейс позволяет подменять реализацию в тестах, но серверная сборка
//! всегда использует Argon2id с параметрами из конфигурации.

use argon2::{Algorithm, Argon2, Params, Version};

use crate::config::Argon2Params;

/// Ошибка хеширования/проверки пароля (детали не раскрываются клиенту).
#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("password hashing failed: {0}")]
    Hash(String),
    #[error("password verification failed: {0}")]
    Verify(String),
}

/// Хеширование и проверка паролей.
pub trait PasswordHasher: Send + Sync + 'static {
    /// Возвращает PHC-строку с уникальной солью и параметрами.
    fn hash(&self, password: &str) -> Result<String, PasswordError>;

    /// Проверяет пароль против сохранённой PHC-строки.
    fn verify(&self, password: &str, phc: &str) -> Result<bool, PasswordError>;
}

/// Argon2id с уникальной солью (PHC-строка).
pub struct Argon2idHasher {
    argon2: Argon2<'static>,
}

impl Argon2idHasher {
    pub fn new(params: Argon2Params) -> Result<Self, PasswordError> {
        let params = Params::new(
            params.memory_kib,
            params.iterations,
            params.parallelism,
            None,
        )
        .map_err(|e| PasswordError::Hash(e.to_string()))?;
        Ok(Self {
            argon2: Argon2::new(Algorithm::Argon2id, Version::V0x13, params),
        })
    }
}

impl PasswordHasher for Argon2idHasher {
    fn hash(&self, password: &str) -> Result<String, PasswordError> {
        // Соль генерируется автоматически (getrandom включён по умолчанию).
        let hash = argon2::PasswordHasher::hash_password(&self.argon2, password.as_bytes())
            .map_err(|e| PasswordError::Hash(e.to_string()))?;
        Ok(hash.to_string())
    }

    fn verify(&self, password: &str, phc: &str) -> Result<bool, PasswordError> {
        match argon2::PasswordVerifier::verify_password(&self.argon2, password.as_bytes(), phc) {
            Ok(()) => Ok(true),
            Err(argon2::password_hash::Error::PasswordInvalid) => Ok(false),
            Err(e) => Err(PasswordError::Verify(e.to_string())),
        }
    }
}

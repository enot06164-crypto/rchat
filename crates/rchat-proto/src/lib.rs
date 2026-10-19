//! Схема сетевого протокола rchat (версия 1), бинарная сериализация
//! на Protocol Buffers, лимиты, валидация и правила совместимости.
//!
//! Крейт не зависит от транспорта, хранилищ, криптографии и приложений.

/// Сгенерированные из `.proto` типы пакета `rchat.proto.v1`.
pub mod proto {
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/rchat.proto.v1.rs"));
}

pub use proto::*;

/// Текущая версия протокола.
pub const PROTOCOL_MAJOR: u32 = 1;
/// minor 1: групповые чаты (Megolm), роли участников, chat_updated.
pub const PROTOCOL_MINOR: u32 = 1;

/// Версии E2E-протокола.
pub const E2E_VERSION_NONE: u32 = 0;
pub const E2E_VERSION_OLM_V1: u32 = 1;
pub const E2E_PROTOCOL_OLM_V1: &str = "olm.v1";

/// Строковые идентификаторы возможностей для capability negotiation.
pub mod caps {
    pub const RECEIPTS: &str = "receipts";
    pub const PRESENCE: &str = "presence";
    pub const EDIT: &str = "edit";
    pub const DELETE: &str = "delete";
    pub const MULTI_DEVICE: &str = "multi_device";
    pub const HISTORY: &str = "history";
    pub const SNAPSHOT: &str = "snapshot";
    pub const E2E_OLM_V1: &str = "e2e.olm.v1";
    /// Групповые чаты: создание/приглашения/выход/исключение и события
    /// chat_updated. Содержимое E2E-групп шифруется Megolm.
    pub const GROUPS: &str = "groups";
    /// Сигнализация звонков (приглашение/принятие/отклонение/завершение
    /// transient-событиями). Медиа не передаётся.
    pub const CALLS: &str = "calls";
}

/// Жёсткие лимиты протокола. Значения по умолчанию конфигурируемы на сервере
/// в разумных пределах; клиент обязан соблюдать жёсткие пределы из этого модуля.
pub mod limits {
    /// Максимальный размер кадра по умолчанию (256 KiB).
    pub const MAX_FRAME_BYTES_DEFAULT: u32 = 256 * 1024;
    /// Абсолютный предел кадра, который принимает схема (1 MiB).
    pub const MAX_FRAME_BYTES_HARD: u32 = 1024 * 1024;
    /// Максимальный размер содержимого сообщения по умолчанию (64 KiB).
    pub const MAX_MESSAGE_BYTES_DEFAULT: u32 = 64 * 1024;
    /// Максимальный размер страницы синхронизации по умолчанию.
    pub const MAX_SYNC_PAGE_DEFAULT: u32 = 200;
    pub const MAX_SYNC_PAGE_HARD: u32 = 500;
    /// Страница истории по умолчанию и жёсткий предел.
    pub const MAX_HISTORY_PAGE_DEFAULT: u32 = 50;
    pub const MAX_HISTORY_PAGE_HARD: u32 = 100;

    pub const USERNAME_MIN: usize = 3;
    pub const USERNAME_MAX: usize = 32;
    pub const DISPLAY_NAME_MIN: usize = 1;
    pub const DISPLAY_NAME_MAX: usize = 64;
    pub const DEVICE_NAME_MAX: usize = 64;
    pub const DEVICE_ID_MAX: usize = 64;
    pub const PASSWORD_MIN: usize = 8;
    pub const PASSWORD_MAX: usize = 256;
    pub const CONTENT_KIND_MAX: usize = 32;
    pub const CLIENT_MESSAGE_ID_MAX: usize = 64;
    pub const CHAT_ID_MAX: usize = 64;
    /// Идентификатор звонка (генерируется сервером).
    pub const CALL_ID_MAX: usize = 64;
    /// Длина nonce облачного шифра (XChaCha20-Poly1305): 24 байта.
    pub const CLOUD_NONCE_LEN: usize = 24;
    /// Длина ключа аккаунта для облачного режима: 32 байта.
    pub const CLOUD_KEY_LEN: usize = 32;
    pub const USER_ID_MAX: usize = 64;
    pub const MAX_CAPABILITIES: usize = 32;
    /// Максимум получателей в одном сообщении (все устройства всех участников).
    /// Поднят с 64 до 128 в protocol minor 1: в группе устройств может быть
    /// больше (участники × устройства), чем в личном чате.
    pub const MAX_RECIPIENTS_PER_MESSAGE: usize = 128;
    /// Максимум prekey в одном запросе загрузки ключей.
    pub const MAX_ONE_TIME_KEYS_PER_REQUEST: usize = 100;
    /// Максимум device_ids в одном запросе пакетов ключей.
    pub const MAX_DEVICE_IDS_PER_REQUEST: usize = 256;
    /// Максимум user_ids в одном запросе списка устройств.
    pub const MAX_USER_IDS_PER_REQUEST: usize = 64;
    /// Участников в личном чате.
    pub const DIRECT_CHAT_MEMBERS: usize = 2;
    /// Название группового чата.
    pub const GROUP_NAME_MIN: usize = 1;
    pub const GROUP_NAME_MAX: usize = 64;
    /// Участников группы: значение по умолчанию и жёсткий предел.
    pub const MAX_GROUP_MEMBERS_DEFAULT: usize = 64;
    pub const MAX_GROUP_MEMBERS_HARD: usize = 128;
    /// Приглашаемых за один запрос.
    pub const MAX_INVITES_PER_REQUEST: usize = 32;
    /// Максимальная длина base64 идентификатора Megolm-сессии.
    pub const GROUP_SESSION_ID_MAX: usize = 128;
    /// Ключи устройства: 32 байта ed25519 и 32 байта curve25519.
    pub const DEVICE_KEY_LEN: usize = 32;
}

/// Нормализация и проверка пользовательских строк.
pub mod validate {
    use super::limits;
    use super::proto::DeviceInfo;

    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    pub enum ValidationError {
        #[error(
            "username length must be {}..={}",
            limits::USERNAME_MIN,
            limits::USERNAME_MAX
        )]
        UsernameLength,
        #[error("username must contain only [a-z0-9_] and start with a letter")]
        UsernameCharset,
        #[error(
            "display name length must be {}..={}",
            limits::DISPLAY_NAME_MIN,
            limits::DISPLAY_NAME_MAX
        )]
        DisplayNameLength,
        #[error(
            "password length must be {}..={}",
            limits::PASSWORD_MIN,
            limits::PASSWORD_MAX
        )]
        PasswordLength,
        #[error("device name too long (max {})", limits::DEVICE_NAME_MAX)]
        DeviceNameTooLong,
        #[error("device id is empty or too long (max {})", limits::DEVICE_ID_MAX)]
        DeviceIdInvalid,
        #[error("device keys must be exactly {} bytes each", limits::DEVICE_KEY_LEN)]
        DeviceKeysInvalid,
        #[error("content kind must be 1..={} ascii chars", limits::CONTENT_KIND_MAX)]
        ContentKindInvalid,
        #[error(
            "client message id must be 1..={} chars",
            limits::CLIENT_MESSAGE_ID_MAX
        )]
        ClientMessageIdInvalid,
        #[error("chat id must be 1..={} chars", limits::CHAT_ID_MAX)]
        ChatIdInvalid,
        #[error("call id must be 1..={} chars", limits::CALL_ID_MAX)]
        CallIdInvalid,
        #[error(
            "group name length must be {}..={}",
            limits::GROUP_NAME_MIN,
            limits::GROUP_NAME_MAX
        )]
        GroupNameLength,
        #[error("user id must be 1..={} chars", limits::USER_ID_MAX)]
        UserIdInvalid,
        #[error("too many recipients: max {}", limits::MAX_RECIPIENTS_PER_MESSAGE)]
        TooManyRecipients,
        #[error(
            "too many one-time keys: max {}",
            limits::MAX_ONE_TIME_KEYS_PER_REQUEST
        )]
        TooManyOneTimeKeys,
        #[error("too many device ids: max {}", limits::MAX_DEVICE_IDS_PER_REQUEST)]
        TooManyDeviceIds,
        #[error("too many user ids: max {}", limits::MAX_USER_IDS_PER_REQUEST)]
        TooManyUserIds,
        #[error("capabilities list too long")]
        TooManyCapabilities,
        #[error("e2e version unsupported: {0}")]
        E2eVersionUnsupported(u32),
    }

    /// Нормализует username (trim + lowercase) и проверяет формат.
    /// Возвращает нормализованное значение.
    pub fn username(raw: &str) -> Result<String, ValidationError> {
        let normalized = raw.trim().to_lowercase();
        let len = normalized.chars().count();
        if !(limits::USERNAME_MIN..=limits::USERNAME_MAX).contains(&len) {
            return Err(ValidationError::UsernameLength);
        }
        let mut chars = normalized.chars();
        match chars.next() {
            Some(c) if c.is_ascii_lowercase() => {}
            _ => return Err(ValidationError::UsernameCharset),
        }
        if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            return Err(ValidationError::UsernameCharset);
        }
        Ok(normalized)
    }

    pub fn display_name(raw: &str) -> Result<String, ValidationError> {
        let trimmed = raw.trim();
        let len = trimmed.chars().count();
        if !(limits::DISPLAY_NAME_MIN..=limits::DISPLAY_NAME_MAX).contains(&len) {
            return Err(ValidationError::DisplayNameLength);
        }
        Ok(trimmed.to_string())
    }

    /// Пароль не нормализуется (пробелы значимы). Проверяется только длина в
    /// байтах UTF-8: сервер хеширует пароль как есть.
    pub fn password(raw: &str) -> Result<(), ValidationError> {
        let len = raw.len();
        if !(limits::PASSWORD_MIN..=limits::PASSWORD_MAX).contains(&len) {
            return Err(ValidationError::PasswordLength);
        }
        Ok(())
    }

    pub fn device_id(raw: &str) -> Result<(), ValidationError> {
        let len = raw.chars().count();
        if len == 0 || len > limits::DEVICE_ID_MAX {
            return Err(ValidationError::DeviceIdInvalid);
        }
        if !raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(ValidationError::DeviceIdInvalid);
        }
        Ok(())
    }

    pub fn content_kind(raw: &str) -> Result<(), ValidationError> {
        let len = raw.len();
        if len == 0 || len > limits::CONTENT_KIND_MAX {
            return Err(ValidationError::ContentKindInvalid);
        }
        if !raw
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_')
        {
            return Err(ValidationError::ContentKindInvalid);
        }
        Ok(())
    }

    pub fn client_message_id(raw: &str) -> Result<(), ValidationError> {
        let len = raw.chars().count();
        if len == 0 || len > limits::CLIENT_MESSAGE_ID_MAX {
            return Err(ValidationError::ClientMessageIdInvalid);
        }
        Ok(())
    }

    pub fn chat_id(raw: &str) -> Result<(), ValidationError> {
        let len = raw.chars().count();
        if len == 0 || len > limits::CHAT_ID_MAX {
            return Err(ValidationError::ChatIdInvalid);
        }
        Ok(())
    }

    pub fn call_id(raw: &str) -> Result<(), ValidationError> {
        let len = raw.chars().count();
        if len == 0 || len > limits::CALL_ID_MAX {
            return Err(ValidationError::CallIdInvalid);
        }
        Ok(())
    }

    /// Нормализует название группового чата (trim) и проверяет длину.
    pub fn group_name(raw: &str) -> Result<String, ValidationError> {
        let trimmed = raw.trim();
        let len = trimmed.chars().count();
        if !(limits::GROUP_NAME_MIN..=limits::GROUP_NAME_MAX).contains(&len) {
            return Err(ValidationError::GroupNameLength);
        }
        Ok(trimmed.to_string())
    }

    pub fn user_id(raw: &str) -> Result<(), ValidationError> {
        let len = raw.chars().count();
        if len == 0 || len > limits::USER_ID_MAX {
            return Err(ValidationError::UserIdInvalid);
        }
        Ok(())
    }

    pub fn capabilities(caps_list: &[String]) -> Result<(), ValidationError> {
        if caps_list.len() > limits::MAX_CAPABILITIES {
            return Err(ValidationError::TooManyCapabilities);
        }
        Ok(())
    }

    /// Проверяет устройство, заявляемое клиентом. `require_keys` — обязательны
    /// ли E2E-ключи (для e2e_version != 0).
    pub fn device_info(info: &DeviceInfo) -> Result<(), ValidationError> {
        device_id(&info.device_id)?;
        if info.name.chars().count() > limits::DEVICE_NAME_MAX {
            return Err(ValidationError::DeviceNameTooLong);
        }
        match info.e2e_version {
            super::E2E_VERSION_NONE => Ok(()),
            super::E2E_VERSION_OLM_V1 => {
                let keys = info
                    .keys
                    .as_ref()
                    .ok_or(ValidationError::DeviceKeysInvalid)?;
                if keys.ed25519.len() != limits::DEVICE_KEY_LEN
                    || keys.curve25519.len() != limits::DEVICE_KEY_LEN
                {
                    return Err(ValidationError::DeviceKeysInvalid);
                }
                Ok(())
            }
            other => Err(ValidationError::E2eVersionUnsupported(other)),
        }
    }
}

// ---------------------------------------------------------------------------
// Кодирование кадров
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("frame exceeds size limit: {size} bytes > {limit} bytes")]
    FrameTooLarge { size: usize, limit: usize },
    #[error("protocol decode error: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("protocol encode error: {0}")]
    Encode(#[from] prost::EncodeError),
    #[error("randomness source unavailable: {0}")]
    Random(String),
}

/// Сериализует envelope без проверки размера (проверка — обязанность отправителя).
pub fn encode_envelope(envelope: &Envelope) -> Result<Vec<u8>, ProtoError> {
    let mut buf = Vec::with_capacity(256);
    prost::Message::encode(envelope, &mut buf)?;
    Ok(buf)
}

/// Декодирует envelope, отклоняя кадры больше `max_frame_bytes`.
pub fn decode_envelope(bytes: &[u8], max_frame_bytes: usize) -> Result<Envelope, ProtoError> {
    if bytes.len() > max_frame_bytes {
        return Err(ProtoError::FrameTooLarge {
            size: bytes.len(),
            limit: max_frame_bytes,
        });
    }
    Ok(prost::Message::decode(bytes)?)
}

/// Проверяет, влезает ли уже сериализованный кадр в лимит.
pub fn check_frame_size(len: usize, max_frame_bytes: usize) -> Result<(), ProtoError> {
    if len > max_frame_bytes {
        return Err(ProtoError::FrameTooLarge {
            size: len,
            limit: max_frame_bytes,
        });
    }
    Ok(())
}

/// Сериализует произвольное protobuf-сообщение протокола (например,
/// сохранённое событие журнала синхронизации).
pub fn encode_message<M: prost::Message>(message: &M) -> Result<Vec<u8>, ProtoError> {
    let mut buf = Vec::with_capacity(256);
    message.encode(&mut buf)?;
    Ok(buf)
}

/// Декодирует protobuf-сообщение протокола.
pub fn decode_message<M: prost::Message + Default>(bytes: &[u8]) -> Result<M, ProtoError> {
    Ok(M::decode(bytes)?)
}

impl Envelope {
    pub fn with_request(request_id: u64, method: request::Method) -> Self {
        Envelope {
            request_id,
            body: Some(envelope::Body::Request(Request {
                method: Some(method),
            })),
        }
    }

    pub fn with_response(request_id: u64, result: response::Result) -> Self {
        Envelope {
            request_id,
            body: Some(envelope::Body::Response(Response {
                result: Some(result),
            })),
        }
    }
}

/// Код ошибки из сообщения `Error`: неизвестное значение → `None`.
/// Сырое значение доступно через поле `code` (prost хранит enum как i32).
pub fn error_code(error: &Error) -> Option<ErrorCode> {
    ErrorCode::try_from(error.code).ok()
}

/// Создаёт типизированную ошибку протокола.
pub fn api_error(code: ErrorCode, message: impl Into<String>, retryable: bool) -> Error {
    Error {
        code: code as i32,
        message: message.into(),
        retryable,
    }
}

/// Обёртка ответа с ошибкой.
pub fn error_response(request_id: u64, error: Error) -> Envelope {
    Envelope::with_response(request_id, response::Result::Error(error))
}

// ---------------------------------------------------------------------------
// Вспомогательные функции, общие для клиента и сервера
// ---------------------------------------------------------------------------

/// Текущее время в миллисекундах Unix (насыщается при переполнении).
pub fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Криптографически случайный идентификатор (hex) из `bytes` байт.
pub fn random_id_hex(bytes: usize) -> Result<String, ProtoError> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| ProtoError::Random(e.to_string()))?;
    Ok(hex_encode(&buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message as _;

    fn sample_register() -> Envelope {
        Envelope::with_request(
            42,
            request::Method::Register(RegisterRequest {
                username: "alice".into(),
                display_name: "Alice".into(),
                password: "hunter2hunter2".into(),
                device: Some(DeviceInfo {
                    device_id: "dev-1".into(),
                    name: "laptop".into(),
                    platform: "linux".into(),
                    keys: Some(DeviceKeys {
                        ed25519: vec![1; 32],
                        curve25519: vec![2; 32],
                    }),
                    e2e_version: E2E_VERSION_OLM_V1,
                    ..Default::default()
                }),
                one_time_keys: vec![PreKey {
                    key_id: 1,
                    public_key: vec![3; 32],
                }],
                fallback_key: None,
            }),
        )
    }

    #[test]
    fn envelope_roundtrip() {
        let env = sample_register();
        let bytes = encode_envelope(&env).expect("encode");
        let decoded =
            decode_envelope(&bytes, limits::MAX_FRAME_BYTES_HARD as usize).expect("decode");
        assert_eq!(env, decoded);
    }

    #[test]
    fn frame_size_limit_enforced() {
        let env = sample_register();
        let bytes = encode_envelope(&env).expect("encode");
        let err = decode_envelope(&bytes, 10).expect_err("must reject");
        assert!(matches!(err, ProtoError::FrameTooLarge { .. }));
        assert!(check_frame_size(bytes.len(), 10).is_err());
        assert!(check_frame_size(bytes.len(), bytes.len()).is_ok());
    }

    /// Старый декодер должен безопасно прочитать сообщение с новым неизвестным
    /// необязательным полем (tag с большим номером) и не потерять известные.
    #[test]
    fn unknown_field_is_ignored() {
        let mut bytes = encode_envelope(&sample_register()).expect("encode");
        // Поле 500, wire type 2 (length-delimited) со строкой "new-data".
        protobuf_nested_field(&mut bytes, 500, b"new-data");

        let decoded =
            decode_envelope(&bytes, limits::MAX_FRAME_BYTES_HARD as usize).expect("decode");
        match decoded.body {
            Some(envelope::Body::Request(req)) => match req.method {
                Some(request::Method::Register(reg)) => {
                    assert_eq!(reg.username, "alice");
                    assert_eq!(reg.one_time_keys.len(), 1);
                }
                other => panic!("unexpected method: {other:?}"),
            },
            other => panic!("unexpected body: {other:?}"),
        }
    }

    /// Кадр, где вместо известного метода стоит неизвестный номер поля,
    /// декодируется в Request без метода — вызывающий код отвечает
    /// UnsupportedMethod, не падая.
    #[test]
    fn unknown_request_method_yields_none() {
        // Request с полем 199 (wire type 2): старый сервер не знает метод.
        let mut payload = Vec::new();
        protobuf_nested_field(&mut payload, 199, b"future-method");
        let mut request_bytes = Vec::new();
        protobuf_nested_field(&mut request_bytes, 4, &payload); // envelope.request = 4

        let mut envelope_bytes = Vec::new();
        protobuf_varint_field(&mut envelope_bytes, 1, 1); // request_id = 1
        envelope_bytes.extend_from_slice(&request_bytes);

        let decoded = decode_envelope(&envelope_bytes, limits::MAX_FRAME_BYTES_HARD as usize)
            .expect("decode");
        assert_eq!(decoded.request_id, 1);
        match decoded.body {
            Some(envelope::Body::Request(req)) => assert!(req.method.is_none()),
            other => panic!("unexpected body: {other:?}"),
        }
    }

    /// Неизвестный код ошибки сохраняется как raw int и не считается валидным enum.
    #[test]
    fn unknown_error_code_is_preserved_raw() {
        let err = Error {
            code: 999,
            message: "future".into(),
            retryable: false,
        };
        let env = error_response(7, err.clone());
        let bytes = encode_envelope(&env).expect("encode");
        let decoded =
            decode_envelope(&bytes, limits::MAX_FRAME_BYTES_HARD as usize).expect("decode");
        match decoded.body {
            Some(envelope::Body::Response(resp)) => match resp.result {
                Some(response::Result::Error(e)) => {
                    assert_eq!(e.code, 999);
                    assert!(error_code(&e).is_none());
                }
                other => panic!("unexpected result: {other:?}"),
            },
            other => panic!("unexpected body: {other:?}"),
        }
    }

    /// Неизвестный тип события: seq и kind_hint доступны, payload отсутствует —
    /// клиент может пропустить событие и продвинуть курсор.
    #[test]
    fn unknown_sync_event_payload_is_skipped_safely() {
        let mut event_bytes = Vec::new();
        protobuf_varint_field(&mut event_bytes, 1, 10); // seq = 10
        protobuf_varint_field(&mut event_bytes, 2, 199); // kind_hint = 199
                                                         // неизвестная полезная нагрузка: поле 199, длина 3
        protobuf_nested_field(&mut event_bytes, 199, b"xyz");

        let event = SyncEvent::decode(&event_bytes[..]).expect("decode sync event");
        assert_eq!(event.seq, 10);
        assert_eq!(event.kind_hint, 199);
        assert!(event.payload.is_none());
    }

    #[test]
    fn validation_username_normalization() {
        assert_eq!(validate::username("  Alice_1 ").unwrap(), "alice_1");
        assert_eq!(
            validate::username("ab"),
            Err(validate::ValidationError::UsernameLength)
        );
        assert_eq!(
            validate::username("1abc"),
            Err(validate::ValidationError::UsernameCharset)
        );
        assert_eq!(
            validate::username("alice-1"),
            Err(validate::ValidationError::UsernameCharset)
        );
        assert!(validate::username("alice").is_ok());
        assert!(validate::username(&"a".repeat(32)).is_ok());
        assert!(validate::username(&"a".repeat(33)).is_err());
    }

    #[test]
    fn validation_group_name() {
        assert_eq!(validate::group_name("  Команда  ").unwrap(), "Команда");
        assert!(validate::group_name("").is_err());
        assert!(validate::group_name("   ").is_err());
        assert!(validate::group_name(&"я".repeat(64)).is_ok());
        assert!(validate::group_name(&"я".repeat(65)).is_err());
    }

    /// Новые поля группового сообщения (minor 1) переживают round-trip.
    #[test]
    fn group_message_roundtrip() {
        let env = Envelope::with_request(
            7,
            request::Method::SendMessage(SendMessageRequest {
                chat_id: "c1".into(),
                client_message_id: "m1".into(),
                content_kind: "text".into(),
                reply_to_server_id: 0,
                recipients: vec![RecipientPayload {
                    device_id: "d1".into(),
                    ciphertext: vec![9; 8],
                    e2e_version: E2E_VERSION_OLM_V1,
                    olm_type: OlmMessageType::Prekey as i32,
                    sender_curve25519: vec![1; 32],
                    payload_kind: RecipientPayloadKind::GroupKey as i32,
                }],
                plaintext_dev: String::new(),
                group_ciphertext: vec![7; 16],
                group_session_id: "sid".into(),
                grouped_id: String::new(),
                cloud_ciphertext: Vec::new(),
                cloud_nonce: Vec::new(),
            }),
        );
        let bytes = encode_envelope(&env).expect("encode");
        let decoded =
            decode_envelope(&bytes, limits::MAX_FRAME_BYTES_HARD as usize).expect("decode");
        assert_eq!(env, decoded);
    }

    #[test]
    fn validation_password_and_device() {
        assert!(validate::password("short").is_err());
        assert!(validate::password("12345678").is_ok());
        assert!(validate::password(&"x".repeat(257)).is_err());

        assert!(validate::device_id("dev-01_x").is_ok());
        assert!(validate::device_id("").is_err());
        assert!(validate::device_id("плохой id").is_err());
    }

    #[test]
    fn validation_device_info_keys() {
        let mut info = DeviceInfo {
            device_id: "d1".into(),
            name: "n".into(),
            platform: "linux".into(),
            keys: None,
            e2e_version: E2E_VERSION_OLM_V1,
            ..Default::default()
        };
        assert!(validate::device_info(&info).is_err());
        info.keys = Some(DeviceKeys {
            ed25519: vec![0; 31],
            curve25519: vec![0; 32],
        });
        assert!(validate::device_info(&info).is_err());
        info.keys = Some(DeviceKeys {
            ed25519: vec![0; 32],
            curve25519: vec![0; 32],
        });
        assert!(validate::device_info(&info).is_ok());
        info.keys = None;
        info.e2e_version = E2E_VERSION_NONE;
        assert!(validate::device_info(&info).is_ok());
        info.e2e_version = 7;
        assert!(validate::device_info(&info).is_err());
    }

    #[test]
    fn random_id_shape() {
        let id = random_id_hex(16).expect("random");
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(id, random_id_hex(16).expect("random"));
    }

    // --- маленькие protobuf-помощники для тестов ---

    fn varint(buf: &mut Vec<u8>, mut value: u64) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                buf.push(byte);
                break;
            }
            buf.push(byte | 0x80);
        }
    }

    fn protobuf_tag(buf: &mut Vec<u8>, field: u64, wire_type: u64) {
        varint(buf, (field << 3) | wire_type);
    }

    fn protobuf_varint_field(buf: &mut Vec<u8>, field: u64, value: u64) {
        protobuf_tag(buf, field, 0);
        varint(buf, value);
    }

    fn protobuf_nested_field(buf: &mut Vec<u8>, field: u64, payload: &[u8]) {
        protobuf_tag(buf, field, 2);
        varint(buf, payload.len() as u64);
        buf.extend_from_slice(payload);
    }
}

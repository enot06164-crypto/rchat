//! Конверт содержимого сообщения: текст + вложение (файл, фото, видео, голос).
//!
//! Формат — компактный JSON в UTF-8, который шифруется целиком
//! (olm/megolm), поэтому байты вложения никогда не покидают устройство в
//! открытом виде: сервер видит только ciphertext.
//!
//! Совместимость: сообщения, отправленные до появления вложений, шифровались
//! как обычный текст. При разборе payload, который не является конвертом,
//! считается текстом (обратная совместимость).

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::{json, Value};

/// Максимальный размер вложения (байт).
///
/// Файл едет отдельным блобом (зашифрованным), поэтому размер больше не
/// ограничен кадром сообщения; ограничение — практическое.
pub const MAX_ATTACHMENT_BYTES: usize = 16 * 1024 * 1024;

/// Максимальный размер имени файла (байт).
pub const MAX_ATTACHMENT_NAME_BYTES: usize = 255;

/// Вид вложения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    /// Фотография.
    Photo,
    /// Видео.
    Video,
    /// Произвольный файл.
    File,
    /// Голосовое сообщение.
    Voice,
}

impl AttachmentKind {
    /// Строковое имя вида (совпадает с `content_kind` сообщения).
    pub fn as_str(self) -> &'static str {
        match self {
            AttachmentKind::Photo => "photo",
            AttachmentKind::Video => "video",
            AttachmentKind::File => "file",
            AttachmentKind::Voice => "voice",
        }
    }

    /// Разбор строкового имени вида.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "photo" => Some(AttachmentKind::Photo),
            "video" => Some(AttachmentKind::Video),
            "file" => Some(AttachmentKind::File),
            "voice" => Some(AttachmentKind::Voice),
            _ => None,
        }
    }
}

/// Вложение сообщения.
///
/// Файл хранится на сервере отдельным блобом: клиент шифрует его случайным
/// ключом (как в Telegram), а в сообщение кладёт только ссылку на блоб и ключ.
/// Сервер видит лишь шифротекст файла.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// Вид вложения.
    pub kind: AttachmentKind,
    /// Имя файла (для голосового — «voice»).
    pub name: String,
    /// MIME-тип.
    pub mime: String,
    /// Размер в байтах.
    pub size: u64,
    /// Ссылка на блоб в хранилище сервера.
    pub blob_ref: Option<String>,
    /// Ключ шифрования файла (внутри конверта; сервер его не видит).
    pub key: Option<Vec<u8>>,
    /// Nonce симметричного шифра файла.
    pub nonce: Option<Vec<u8>>,
    /// Содержимое файла: только для сообщений, отправленных до появления
    /// канала блобов (файл ехал внутри сообщения).
    pub data: Vec<u8>,
}

impl Attachment {
    /// Проверка вложения перед отправкой.
    pub fn validate(&self) -> Result<(), String> {
        if self.size as usize > MAX_ATTACHMENT_BYTES {
            return Err(format!(
                "attachment too large: {} > {MAX_ATTACHMENT_BYTES} bytes",
                self.size
            ));
        }
        if self.data.len() > MAX_ATTACHMENT_BYTES {
            return Err(format!(
                "attachment too large: {} > {MAX_ATTACHMENT_BYTES} bytes",
                self.data.len()
            ));
        }
        if self.name.len() > MAX_ATTACHMENT_NAME_BYTES {
            return Err(format!(
                "attachment name too long: {} > {MAX_ATTACHMENT_NAME_BYTES} bytes",
                self.name.len()
            ));
        }
        Ok(())
    }
}

/// Конверт сообщения: текст и (необязательное) вложение.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Payload {
    /// Текст сообщения (может быть пустым, если есть вложение).
    pub text: String,
    /// Вложение (файл, фото, видео, голос).
    pub attachment: Option<Attachment>,
}

/// Магический маркер конверта (первый элемент массива JSON).
const MAGIC: &str = "RCP1";

impl Payload {
    /// Конверт только с текстом.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            attachment: None,
        }
    }

    /// Конверт с текстом и вложением.
    pub fn with_attachment(text: impl Into<String>, attachment: Attachment) -> Self {
        Self {
            text: text.into(),
            attachment: Some(attachment),
        }
    }

    /// `content_kind` сообщения: вид вложения либо `text`.
    pub fn content_kind(&self) -> String {
        match &self.attachment {
            Some(attachment) => attachment.kind.as_str().to_string(),
            None => "text".to_string(),
        }
    }

    /// Кодирование конверта в байты (UTF-8 JSON).
    pub fn encode(&self) -> Vec<u8> {
        let mut attachment = serde_json::Map::new();
        if let Some(item) = &self.attachment {
            attachment.insert("k".into(), json!(item.kind.as_str()));
            attachment.insert("n".into(), json!(item.name));
            attachment.insert("m".into(), json!(item.mime));
            attachment.insert("s".into(), json!(item.size));
            if let Some(reference) = &item.blob_ref {
                attachment.insert("b".into(), json!(reference));
            }
            if let Some(key) = &item.key {
                attachment.insert("key".into(), json!(BASE64.encode(key)));
            }
            if let Some(nonce) = &item.nonce {
                attachment.insert("nonce".into(), json!(BASE64.encode(nonce)));
            }
            if !item.data.is_empty() {
                attachment.insert("d".into(), json!(BASE64.encode(&item.data)));
            }
        }
        let mut body = serde_json::Map::new();
        body.insert("t".into(), json!(self.text));
        if !attachment.is_empty() {
            body.insert("a".into(), Value::Object(attachment));
        }
        let value = json!([MAGIC, Value::Object(body)]);
        serde_json::to_vec(&value).unwrap_or_else(|_| self.text.as_bytes().to_vec())
    }

    /// Разбор байтов payload: конверт либо (для старых сообщений) текст.
    pub fn decode(bytes: &[u8]) -> Payload {
        match Self::decode_envelope(bytes) {
            Some(payload) => payload,
            None => Payload::text(String::from_utf8_lossy(bytes).into_owned()),
        }
    }

    /// Разбор конверта; `None`, если байты не конверт.
    fn decode_envelope(bytes: &[u8]) -> Option<Payload> {
        let value: Value = serde_json::from_slice(bytes).ok()?;
        let array = value.as_array()?;
        if array.len() != 2 || array[0].as_str()? != MAGIC {
            return None;
        }
        let body = array[1].as_object()?;
        let text = body
            .get("t")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let attachment = body.get("a").and_then(|item| {
            let item = item.as_object()?;
            let kind = AttachmentKind::parse(item.get("k")?.as_str()?)?;
            let name = item
                .get("n")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let mime = item
                .get("m")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let size = item.get("s").and_then(Value::as_u64).unwrap_or(0);
            let blob_ref = item.get("b").and_then(Value::as_str).map(str::to_string);
            let key = item
                .get("key")
                .and_then(Value::as_str)
                .map(|raw| BASE64.decode(raw).unwrap_or_default());
            let nonce = item
                .get("nonce")
                .and_then(Value::as_str)
                .map(|raw| BASE64.decode(raw).unwrap_or_default());
            let data = item
                .get("d")
                .and_then(Value::as_str)
                .map(|raw| BASE64.decode(raw).unwrap_or_default())
                .unwrap_or_default();
            Some(Attachment {
                kind,
                name,
                mime,
                size,
                blob_ref,
                key,
                nonce,
                data,
            })
        });
        Some(Payload { text, attachment })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_only_round_trip() {
        let payload = Payload::text("привет");
        let decoded = Payload::decode(&payload.encode());
        assert_eq!(decoded.text, "привет");
        assert!(decoded.attachment.is_none());
        assert_eq!(decoded.content_kind(), "text");
    }

    #[test]
    fn attachment_round_trip() {
        let attachment = Attachment {
            kind: AttachmentKind::Photo,
            name: "pic.png".into(),
            mime: "image/png".into(),
            size: 3,
            blob_ref: None,
            key: None,
            nonce: None,
            data: vec![1, 2, 3],
        };
        let payload = Payload::with_attachment("смотри", attachment.clone());
        let decoded = Payload::decode(&payload.encode());
        assert_eq!(decoded.text, "смотри");
        assert_eq!(decoded.attachment, Some(attachment));
        assert_eq!(decoded.content_kind(), "photo");
    }

    #[test]
    fn legacy_plain_text_is_kept() {
        // Старое сообщение — это просто текст, без конверта.
        let decoded = Payload::decode("просто текст".as_bytes());
        assert_eq!(decoded.text, "просто текст");
        assert!(decoded.attachment.is_none());
    }

    #[test]
    fn legacy_json_text_is_not_mistaken_for_envelope() {
        // Текст, случайно похожий на JSON, не должен считаться конвертом.
        let legacy = b"{\"v\":1,\"t\":\"hello\"}";
        let decoded = Payload::decode(legacy);
        assert_eq!(decoded.text, "{\"v\":1,\"t\":\"hello\"}");
        assert!(decoded.attachment.is_none());
    }

    #[test]
    fn voice_kind_parsed() {
        assert_eq!(AttachmentKind::parse("voice"), Some(AttachmentKind::Voice));
        assert_eq!(AttachmentKind::parse("sticker"), None);
    }
}

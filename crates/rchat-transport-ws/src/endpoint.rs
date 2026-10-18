//! Разбор `ws://`/`wss://` URL в параметры соединения.

use rchat_core::error::TransportError;

/// Разобранный адрес сервера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// `true` для `wss://`.
    pub secure: bool,
    /// Имя хоста (без скобок у IPv6) или IP-адрес.
    pub host: String,
    pub port: u16,
    /// Абсолютный URI для WebSocket-handshake; путь всегда непустой.
    pub uri: String,
}

impl Endpoint {
    /// Разбирает URL сервера. Поддерживаются только схемы `ws` и `wss`.
    ///
    /// Основной источник URL — конфигурация CLI, поэтому синтаксис тот же,
    /// что у WebSocket-URL браузера: схема, хост (DNS, IPv4 или IPv6 в
    /// квадратных скобках), необязательный порт, необязательный путь/query.
    pub fn parse(url: &str) -> Result<Self, TransportError> {
        let trimmed = url.trim();
        let (scheme, rest) = trimmed.split_once("://").ok_or_else(|| {
            TransportError::connect(format!(
                "URL сервера без схемы: {trimmed:?} (ожидается ws:// или wss://)"
            ))
        })?;
        let secure = match scheme.to_ascii_lowercase().as_str() {
            "ws" => false,
            "wss" => true,
            other => {
                return Err(TransportError::connect(format!(
                    "схема {other:?} не поддерживается (ожидается ws:// или wss://)"
                )))
            }
        };
        let default_port = if secure { 443 } else { 80 };

        // authority — до первого разделителя пути/запроса/фрагмента.
        let split = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..split];
        let mut path = &rest[split..];
        if authority.is_empty() {
            return Err(TransportError::connect(format!(
                "в URL {trimmed:?} отсутствует хост"
            )));
        }
        if let Some(hash) = path.find('#') {
            path = &path[..hash];
        }
        let (host, port) = split_authority(authority, default_port)?;
        let path = if path.is_empty() {
            "/".to_string()
        } else if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        let scheme = if secure { "wss" } else { "ws" };
        let uri = if host.contains(':') {
            format!("{scheme}://[{host}]:{port}{path}")
        } else {
            format!("{scheme}://{host}:{port}{path}")
        };
        Ok(Self {
            secure,
            host,
            port,
            uri,
        })
    }
}

fn split_authority(authority: &str, default_port: u16) -> Result<(String, u16), TransportError> {
    if authority.contains('@') {
        return Err(TransportError::connect(
            "URL-авторизация (user@host) не поддерживается".to_string(),
        ));
    }
    if let Some(rest) = authority.strip_prefix('[') {
        let close = rest.find(']').ok_or_else(|| {
            TransportError::connect("в IPv6-адресе не закрыта скобка '['".to_string())
        })?;
        let host = &rest[..close];
        if host.is_empty() {
            return Err(TransportError::connect("пустой IPv6-адрес".to_string()));
        }
        let after = &rest[close + 1..];
        let port = match after.strip_prefix(':') {
            Some(value) => parse_port(value)?,
            None if after.is_empty() => default_port,
            None => {
                return Err(TransportError::connect(format!(
                    "некорректный адрес {authority:?}: ожидался ':' после ']'"
                )))
            }
        };
        return Ok((host.to_string(), port));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, value)) => (host, parse_port(value)?),
        None => (authority, default_port),
    };
    if host.is_empty() {
        return Err(TransportError::connect("в URL пустой хост".to_string()));
    }
    Ok((host.to_string(), port))
}

fn parse_port(value: &str) -> Result<u16, TransportError> {
    let port: u16 = value.parse().map_err(|_| {
        TransportError::connect(format!("некорректный порт {value:?} в URL сервера"))
    })?;
    if port == 0 {
        return Err(TransportError::connect("порт 0 недопустим".to_string()));
    }
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_ws() {
        let endpoint = Endpoint::parse("ws://127.0.0.1:8443").expect("endpoint");
        assert!(!endpoint.secure);
        assert_eq!(endpoint.host, "127.0.0.1");
        assert_eq!(endpoint.port, 8443);
        assert_eq!(endpoint.uri, "ws://127.0.0.1:8443/");
    }

    #[test]
    fn parses_wss_with_path_and_query() {
        let endpoint = Endpoint::parse("wss://chat.example.com/api?x=1").expect("endpoint");
        assert!(endpoint.secure);
        assert_eq!(endpoint.host, "chat.example.com");
        assert_eq!(endpoint.port, 443);
        assert_eq!(endpoint.uri, "wss://chat.example.com:443/api?x=1");
    }

    #[test]
    fn parses_ipv6_with_port() {
        let endpoint = Endpoint::parse("ws://[::1]:9000/ws").expect("endpoint");
        assert_eq!(endpoint.host, "::1");
        assert_eq!(endpoint.port, 9000);
        assert_eq!(endpoint.uri, "ws://[::1]:9000/ws");
    }

    #[test]
    fn default_ports() {
        assert_eq!(Endpoint::parse("ws://host/a").expect("ws").port, 80);
        assert_eq!(Endpoint::parse("wss://host/a").expect("wss").port, 443);
    }

    #[test]
    fn strips_fragment() {
        let endpoint = Endpoint::parse("ws://host:1/a#frag").expect("endpoint");
        assert_eq!(endpoint.uri, "ws://host:1/a");
    }

    #[test]
    fn rejects_bad_scheme() {
        assert!(Endpoint::parse("http://host:1").is_err());
        assert!(Endpoint::parse("host:1").is_err());
        assert!(Endpoint::parse("ws://").is_err());
        assert!(Endpoint::parse("ws://host:0").is_err());
        assert!(Endpoint::parse("ws://user@host:1").is_err());
        assert!(Endpoint::parse("ws://[::1:9000").is_err());
    }
}

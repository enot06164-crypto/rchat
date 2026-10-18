//! TLS-доверие к серверу: системные корни, пиннинг отпечатка, явный небезопасный режим.

use std::sync::Arc;

use rchat_core::error::{TransportError, TransportErrorKind};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error as TlsError, SignatureScheme};
use sha2::{Digest, Sha256};

/// Режим проверки TLS-сертификата сервера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TlsTrust {
    /// Проверка по системным корневым сертификатам (боевой режим).
    SystemRoots,
    /// Пиннинг SHA-256 отпечатка листового сертификата сервера.
    ///
    /// Применяется для локальной разработки и развёртываний с
    /// самоподписанными сертификатами: имя хоста при этом не проверяется
    /// (отпечаток строго фиксирует конкретный сертификат).
    Fingerprint([u8; 32]),
    /// Проверка сертификата отключена. Допустимо только в явно локальных
    /// сценариях (например, ручная проверка `ws://` c проксированным TLS);
    /// боевой клиент использовать этот режим не должен.
    Insecure,
}

impl TlsTrust {
    /// Разбирает SHA-256 отпечаток в hex. Разделители `:`, `-`, `_` и
    /// пробелы допускаются и игнорируются.
    pub fn fingerprint_hex(input: &str) -> Result<Self, TransportError> {
        let mut digits = String::with_capacity(64);
        for ch in input.chars() {
            match ch {
                ':' | '-' | '_' | ' ' | '\t' => continue,
                ch if ch.is_ascii_hexdigit() => digits.push(ch),
                other => {
                    return Err(TransportError::new(
                        TransportErrorKind::Other,
                        format!("недопустимый символ {other:?} в отпечатке сертификата"),
                    ))
                }
            }
        }
        if digits.len() != 64 {
            return Err(TransportError::new(
                TransportErrorKind::Other,
                format!(
                    "отпечаток сертификата должен содержать 64 hex-символа, получено {}",
                    digits.len()
                ),
            ));
        }
        let raw = digits.as_bytes();
        let mut out = [0u8; 32];
        for (index, chunk) in raw.chunks(2).enumerate() {
            let pair = std::str::from_utf8(chunk).map_err(|_| {
                TransportError::new(TransportErrorKind::Other, "некорректный hex отпечатка")
            })?;
            out[index] = u8::from_str_radix(pair, 16).map_err(|_| {
                TransportError::new(TransportErrorKind::Other, "некорректный hex отпечатка")
            })?;
        }
        Ok(Self::Fingerprint(out))
    }
}

/// SHA-256 отпечаток DER-сертификата: именно это значение сравнивается в
/// режиме [`TlsTrust::Fingerprint`] и печатается операторами при настройке
/// сервера.
pub fn certificate_fingerprint(cert_der: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(cert_der);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// Форматирует отпечаток в виде строки `aa:bb:...` (для логов и документации).
pub fn format_fingerprint(fingerprint: &[u8; 32]) -> String {
    let mut out = String::with_capacity(95);
    for (index, byte) in fingerprint.iter().enumerate() {
        if index > 0 {
            out.push(':');
        }
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub(crate) fn client_config(trust: &TlsTrust) -> Result<ClientConfig, TransportError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .map_err(|error| TransportError::connect(format!("TLS-конфигурация: {error}")))?;
    match trust {
        TlsTrust::SystemRoots => {
            let mut roots = rustls::RootCertStore::empty();
            let loaded = rustls_native_certs::load_native_certs();
            for error in &loaded.errors {
                tracing::warn!(%error, "не удалось прочитать системный корневой сертификат");
            }
            let (added, ignored) = roots.add_parsable_certificates(loaded.certs);
            if ignored > 0 {
                tracing::warn!(ignored, "часть системных сертификатов не разобрана");
            }
            if added == 0 {
                return Err(TransportError::connect(
                    "в системе не найдено ни одного корневого сертификата TLS",
                ));
            }
            Ok(builder.with_root_certificates(roots).with_no_client_auth())
        }
        TlsTrust::Fingerprint(expected) => {
            let verifier = Arc::new(FingerprintVerifier {
                expected: *expected,
                provider,
            });
            Ok(builder
                .dangerous()
                .with_custom_certificate_verifier(verifier)
                .with_no_client_auth())
        }
        TlsTrust::Insecure => {
            tracing::warn!(
                "проверка TLS-сертификата отключена (TlsTrust::Insecure): только для локального тестирования"
            );
            let verifier = Arc::new(InsecureVerifier { provider });
            Ok(builder
                .dangerous()
                .with_custom_certificate_verifier(verifier)
                .with_no_client_auth())
        }
    }
}

#[derive(Debug)]
struct FingerprintVerifier {
    expected: [u8; 32],
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for FingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let actual = certificate_fingerprint(end_entity.as_ref());
        if actual == self.expected {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(TlsError::General(format!(
                "отпечаток сертификата сервера не совпал: ожидался {}, получен {}",
                format_fingerprint(&self.expected),
                format_fingerprint(&actual)
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[derive(Debug)]
struct InsecureVerifier {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for InsecureVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fingerprint_with_separators() {
        let mut text = String::new();
        for (index, byte) in (0u8..32).enumerate() {
            if index > 0 {
                text.push(match index % 3 {
                    0 => ':',
                    1 => '-',
                    _ => ' ',
                });
            }
            if index % 2 == 0 {
                text.push_str(&format!("{byte:02X}"));
            } else {
                text.push_str(&format!("{byte:02x}"));
            }
        }
        let trust = TlsTrust::fingerprint_hex(&text).expect("fingerprint");
        let expected: [u8; 32] = std::array::from_fn(|index| index as u8);
        assert_eq!(trust, TlsTrust::Fingerprint(expected));
    }

    #[test]
    fn rejects_bad_fingerprints() {
        assert!(TlsTrust::fingerprint_hex("zz").is_err());
        assert!(TlsTrust::fingerprint_hex("aa:bb").is_err());
        assert!(TlsTrust::fingerprint_hex(&"00".repeat(33)).is_err());
    }

    #[test]
    fn formats_fingerprint_roundtrip() {
        let bytes = [0u8; 32];
        let text = format_fingerprint(&bytes);
        assert_eq!(text.len(), 95);
        assert_eq!(
            TlsTrust::fingerprint_hex(&text).expect("parse"),
            TlsTrust::Fingerprint(bytes)
        );
    }
}

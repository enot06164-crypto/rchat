//! Бинарник сервера rchat: аргументы командной строки, TLS, WebSocket и
//! запуск gateway.
//!
//! Примеры запуска:
//!
//! ```text
//! # Локальная разработка: TLS с самоподписанным сертификатом (по умолчанию).
//! rchat-server --db rchat.sqlite3 --listen 127.0.0.1:8443
//!
//! # Явный небезопасный режим без TLS — запрещён вне loopback.
//! rchat-server --db rchat.sqlite3 --listen 127.0.0.1:8787 --plaintext-insecure
//!
//! # Рабочий режим с сертификатами.
//! rchat-server --db rchat.sqlite3 --listen 0.0.0.0:8443 \
//!     --tls-cert cert.pem --tls-key key.pem
//! ```
//!
//! TLS обязателен вне явно локального тестирования: режим
//! `--plaintext-insecure` отвергается для адресов, отличных от loopback.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use rchat_server::{Argon2idHasher, Server, ServerConfig};
use rchat_server_storage_sqlite::net::{
    handle_client, is_loopback, tls_dev_self_signed, tls_from_files, ws_config,
};
use rchat_server_storage_sqlite::SqliteRepo;
use tokio::net::TcpListener;

/// Выбранный режим транспортной безопасности.
enum TlsChoice {
    /// Сертификат и ключ из файлов.
    Files { cert: PathBuf, key: PathBuf },
    /// Самоподписанный сертификат, сгенерированный на старте (разработка).
    DevSelfSigned { hosts: Vec<String> },
    /// Без TLS; допускается только для loopback.
    PlaintextInsecure,
}

struct Args {
    listen: SocketAddr,
    db: PathBuf,
    tls: TlsChoice,
    allow_dev_plaintext_chats: bool,
    server_name: Option<String>,
}

const USAGE: &str = "\
rchat-server — сервер мессенджера rchat

Использование:
  rchat-server [--listen ADDR] [--db FILE] [РЕЖИМ TLS] [--options]

Основные параметры:
  --listen ADDR            адрес прослушивания (по умолчанию 127.0.0.1:8443)
  --db FILE                файл базы SQLite (по умолчанию rchat.sqlite3)
  --server-name NAME       имя сервера в ServerHello

Транспортная безопасность (по умолчанию TLS с самоподписанным сертификатом):
  --tls-cert FILE --tls-key FILE   TLS с сертификатом и ключом в PEM
  --dev-self-signed                самоподписанный сертификат для разработки
  --dev-host HOST                  дополнительное имя в сертификате (можно
                                   повторять; localhost и loopback всегда)
  --plaintext-insecure             БЕЗ TLS (только loopback, небезопасно)

Режимы данных:
  --allow-dev-plaintext-chats      разрешить чаты E2E_MODE_NONE_DEV
                                   (явный режим разработки, не E2E)

  -h, --help                       эта справка
";

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut listen: Option<SocketAddr> = None;
    let mut db: Option<PathBuf> = None;
    let mut cert: Option<PathBuf> = None;
    let mut key: Option<PathBuf> = None;
    let mut dev_self_signed = false;
    let mut plaintext = false;
    let mut dev_hosts: Vec<String> = Vec::new();
    let mut allow_dev_plaintext_chats = false;
    let mut server_name: Option<String> = None;

    while let Some(arg) = args.next() {
        let mut value_for = |name: &str| -> Result<String, String> {
            args.next()
                .ok_or_else(|| format!("не указано значение для {name}"))
        };
        match arg.as_str() {
            "-h" | "--help" => return Err(String::new()),
            "--listen" => {
                let value = value_for("--listen")?;
                listen = Some(
                    value
                        .parse()
                        .map_err(|_| format!("некорректный адрес: {value}"))?,
                );
            }
            "--db" => db = Some(PathBuf::from(value_for("--db")?)),
            "--tls-cert" => cert = Some(PathBuf::from(value_for("--tls-cert")?)),
            "--tls-key" => key = Some(PathBuf::from(value_for("--tls-key")?)),
            "--dev-self-signed" => dev_self_signed = true,
            "--dev-host" => dev_hosts.push(value_for("--dev-host")?),
            "--plaintext-insecure" => plaintext = true,
            "--allow-dev-plaintext-chats" => allow_dev_plaintext_chats = true,
            "--server-name" => server_name = Some(value_for("--server-name")?),
            other => return Err(format!("неизвестный параметр: {other}")),
        }
    }

    let tls = match (cert, key, dev_self_signed, plaintext) {
        (Some(cert), Some(key), false, false) => TlsChoice::Files { cert, key },
        (None, None, _, true) if !dev_self_signed => TlsChoice::PlaintextInsecure,
        (None, None, _, false) => TlsChoice::DevSelfSigned { hosts: dev_hosts },
        _ => {
            return Err(
                "выберите один режим TLS: --tls-cert/--tls-key, --dev-self-signed \
                 или --plaintext-insecure"
                    .to_string(),
            )
        }
    };

    Ok(Args {
        listen: listen.unwrap_or_else(|| "127.0.0.1:8443".parse().expect("valid default")),
        db: db.unwrap_or_else(|| PathBuf::from("rchat.sqlite3")),
        tls,
        allow_dev_plaintext_chats,
        server_name,
    })
}

fn app_error(message: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message)
}

async fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = ServerConfig {
        allow_dev_plaintext_chats: args.allow_dev_plaintext_chats,
        ..ServerConfig::default()
    };
    if let Some(name) = args.server_name {
        config.server_name = name;
    }

    // Режим транспорта: собираем acceptor заранее, чтобы падать до bind.
    let acceptor = match &args.tls {
        TlsChoice::Files { cert, key } => Some(tokio_rustls::TlsAcceptor::from(Arc::new(
            tls_from_files(cert, key).map_err(app_error)?,
        ))),
        TlsChoice::DevSelfSigned { hosts } => {
            let (dev_config, fingerprint) = tls_dev_self_signed(hosts).map_err(app_error)?;
            tracing::warn!(
                fingerprint = %fingerprint,
                "режим разработки: самоподписанный TLS-сертификат; \
                 клиенту нужен --tls-fingerprint <отпечаток> (или --tls-insecure)"
            );
            Some(tokio_rustls::TlsAcceptor::from(Arc::new(dev_config)))
        }
        TlsChoice::PlaintextInsecure => {
            if !is_loopback(&args.listen.ip()) {
                return Err(app_error(format!(
                    "--plaintext-insecure допускается только для loopback, а не {}",
                    args.listen
                ))
                .into());
            }
            tracing::warn!(
                addr = %args.listen,
                "явный небезопасный режим: соединения без TLS (только локальное тестирование)"
            );
            None
        }
    };

    let repo = SqliteRepo::open(&args.db)
        .map_err(|e| app_error(format!("не удалось открыть БД {}: {e}", args.db.display())))?;
    tracing::info!(db = %args.db.display(), "хранилище открыто");

    let server = Arc::new(
        Server::start(
            Arc::new(repo),
            config.clone(),
            Arc::new(
                Argon2idHasher::new(config.argon2)
                    .map_err(|e| app_error(format!("argon2: {e}")))?,
            ),
        )
        .await?,
    );

    let listener = TcpListener::bind(args.listen).await?;
    let local_addr = listener.local_addr()?;
    tracing::info!(
        addr = %local_addr,
        tls = acceptor.is_some(),
        server_name = %config.server_name,
        "сервер слушает"
    );

    let ws_config = ws_config(config.max_frame_bytes);
    loop {
        let accept = tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("остановка по сигналу");
                break;
            }
            result = listener.accept() => result,
        };
        match accept {
            Ok((stream, _addr)) => {
                let server = server.clone();
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_client(stream, acceptor, server, ws_config).await {
                        tracing::debug!(error = %error, "соединение завершилось ошибкой");
                    }
                });
            }
            Err(error) => {
                tracing::warn!(error = %error, "ошибка accept");
            }
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            if message.is_empty() {
                println!("{USAGE}");
                return;
            }
            eprintln!("rchat-server: {message}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(error) = run(args).await {
        eprintln!("rchat-server: {error}");
        std::process::exit(1);
    }
}

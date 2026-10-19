//! Тест-«замок» на правила зависимостей ядра.
//!
//! Ядро (`rchat-core`) обязано оставаться свободным от конкретных
//! реализаций: транспорта (tungstenite), баз данных (sqlx, rusqlite),
//! HTTP-фреймворков, парсеров CLI (clap), криптографических библиотек и
//! реализаций хранилища секретов. Всё это подключается крейтами-адаптерами.
//!
//! Тест читает собственный манифест и падает, если запрещённая
//! зависимость появилась в любом разделе (включая dev/build-зависимости:
//! тестовая криптография в обычной сборке ядра не допускается).

/// Имена крейтов, которые ядру запрещено подключать напрямую.
const FORBIDDEN_CRATES: &[&str] = &[
    // Транспорт.
    "tungstenite",
    "tokio-tungstenite",
    // Хранилища (SQL и любые встраиваемые СУБД).
    "sqlx",
    "rusqlite",
    "libsqlite3-sys",
    "sled",
    // HTTP-фреймворки и клиенты.
    "axum",
    "actix-web",
    "actix-rt",
    "warp",
    "rocket",
    "hyper",
    "reqwest",
    "ureq",
    // CLI.
    "clap",
    "argh",
    "structopt",
    // Криптографические библиотеки (E2E подключается адаптером).
    "vodozemac",
    "libsignal-protocol",
    "ring",
    "rustls",
    "tokio-rustls",
    "openssl",
    "native-tls",
    "chacha20poly1305",
    "aes-gcm",
    "x25519-dalek",
    "ed25519-dalek",
    "curve25519-dalek",
    // Реализации хранилища секретов.
    "keyring",
    "secret-service",
    "security-framework",
    // Клиентские каталоги и ввод пароля — тоже уровень приложения.
    "directories",
    "rpassword",
];

/// Собственный манифест крейта: `include_str!` фиксируется на этапе сборки.
const MANIFEST: &str = include_str!("../Cargo.toml");

#[test]
fn core_manifest_has_no_forbidden_dependencies() {
    let mut violations = Vec::new();
    for (index, raw_line) in MANIFEST.lines().enumerate() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        // Интересуют только присваивания крейтам: `name = ...`.
        let Some((key, _)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"');
        // `foo.workspace = true` — имя зависимости стоит до точки.
        let crate_name = key.split('.').next().unwrap_or(key);
        if FORBIDDEN_CRATES.contains(&crate_name) {
            violations.push(format!("строка {}: {crate_name}", index + 1));
        }
    }
    assert!(
        violations.is_empty(),
        "ядру запрещено зависеть от: {}",
        violations.join(", ")
    );
}

#[test]
fn core_manifest_dependencies_are_minimal() {
    // Страховка от случайного разрастания: список прямых зависимостей
    // ядра должен оставаться маленьким и осознанным.
    const ALLOWED: &[&str] = &[
        "rchat-proto",
        "async-trait",
        "base64", // кодирование содержимого вложения в конверт сообщения
        "getrandom",
        "prost",
        "serde_json", // конверт сообщения (текст + вложение) в UTF-8 JSON
        "thiserror",
        "tokio",
        "tracing",
        "tempfile", // dev-dependency для тестов хранилищ
    ];
    let mut in_dependencies_section = false;
    for (index, raw_line) in MANIFEST.lines().enumerate() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            in_dependencies_section = line.ends_with("dependencies]");
            continue;
        }
        if !in_dependencies_section {
            continue;
        }
        let Some((key, _)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"');
        let crate_name = key.split('.').next().unwrap_or(key);
        assert!(
            ALLOWED.contains(&crate_name),
            "неожиданная зависимость ядра в строке {}: {crate_name} — добавьте её в белый список осознанно",
            index + 1
        );
    }
}

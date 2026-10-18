//! Терминальный клиент rchat.
//!
//! Бинарник — composition root: он собирает ядро (`rchat-core`) с
//! WebSocket-транспортом (`rchat-transport-ws`), SQLite-хранилищем
//! (`rchat-client-storage-sqlite`) и шифрованным файловым хранилищем
//! секретов. Вся протокольная логика (сессии, синхронизация, outbox) живёт
//! в ядре; здесь только разбор аргументов, вывод и ожидание результатов.

mod app;
mod cli;
mod format;

use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    init_tracing(cli.verbose);
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("не удалось создать tokio runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(app::run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ошибка: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Логи библиотек пишутся в stderr: stdout остаётся машиночитаемым выводом
/// команд. Уровень задаётся флагом `-v`/`-vv` или переменной `RCHAT_LOG`.
fn init_tracing(verbose: u8) {
    let level = match verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("RCHAT_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(format!("rchat={level},warn")));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

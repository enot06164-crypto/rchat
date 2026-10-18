//! Composition root CLI: собирает клиентское ядро с конкретными транспортом,
//! хранилищем и хранилищем секретов, и реализует команды.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rchat_client_storage_sqlite::{ClientSqliteStorage, EncryptedFileSecretStore};
use rchat_core::client::{Client, ClientBuilder};
use rchat_core::config::ClientConfig;
use rchat_core::events::{EventError, UiEvent};
use rchat_core::traits::{secret_keys, SecretStore, Storage};
use rchat_core::types::{ChatRecord, MessageStatus, SyncProgress};
use rchat_crypto_provider::VodozemacCryptoProvider;
use rchat_transport_ws::{TlsTrust, WsTransportConfig, WsTransportFactory};
use zeroize::Zeroizing;

use crate::cli::{Cli, Command, PasswordArgs};
use crate::format;

/// Ошибки CLI: сообщения уже сформулированы для человека, поэтому
/// используется единый boxed-тип.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Собранный клиент и его зависимости, нужные командам напрямую.
struct Context {
    client: Client,
    storage: Arc<ClientSqliteStorage>,
    secrets: Arc<EncryptedFileSecretStore>,
    data_dir: PathBuf,
    server: String,
    tls: TlsTrust,
}

/// Точка входа приложения: собирает клиент и выполняет команду.
pub async fn run(cli: Cli) -> Result<()> {
    let tls = resolve_tls(&cli)?;
    let data_dir = resolve_data_dir(&cli);
    std::fs::create_dir_all(&data_dir)
        .map_err(|error| format!("каталог данных {}: {error}", data_dir.display()))?;

    let storage = Arc::new(ClientSqliteStorage::open(data_dir.join("client.sqlite3"))?);
    let secrets = Arc::new(EncryptedFileSecretStore::open(
        data_dir.join("secrets.bin"),
        data_dir.join("secrets.key"),
    )?);
    let transport = WsTransportFactory::new(WsTransportConfig {
        url: cli.server.clone(),
        tls: tls.clone(),
        ..Default::default()
    })?;
    // E2E включён всегда: провайдер Olm использует штатные pickle-формы
    // vodozemac, ключ шифрования pickle — в хранилище секретов. Чаты без E2E
    // создаются только явно (chat-create без --e2e).
    let crypto = Arc::new(VodozemacCryptoProvider::new(
        storage.clone(),
        secrets.clone(),
    ));
    let config = ClientConfig {
        device_name: resolve_device_name(&cli),
        client_name: "rchat-cli".to_string(),
        e2e_enabled: true,
        ..Default::default()
    };
    let client = ClientBuilder::new(
        config,
        Arc::new(transport),
        storage.clone(),
        secrets.clone(),
    )
    .crypto_provider(crypto)
    .build()?;

    let ctx = Context {
        client,
        storage,
        secrets,
        data_dir,
        server: cli.server.clone(),
        tls,
    };
    let result = dispatch(&ctx, cli.command).await;
    // Аккуратное завершение фонового актора; ошибки завершения не должны
    // скрывать результат команды.
    if let Err(error) = ctx.client.shutdown().await {
        tracing::debug!(%error, "shutdown failed");
    }
    result
}

async fn dispatch(ctx: &Context, command: Command) -> Result<()> {
    match command {
        Command::Register {
            username,
            display_name,
            password,
        } => {
            let display_name = display_name.unwrap_or_else(|| username.clone());
            let password = read_password(&password, true)?;
            ctx.client
                .register(&username, &display_name, &password)
                .await?;
            println!("Зарегистрирован и выполнен вход: {username}");
            print_session(ctx);
            Ok(())
        }
        Command::Login { username, password } => {
            let password = read_password(&password, false)?;
            ctx.client.login(&username, &password).await?;
            println!("Вход выполнен: {username}");
            print_session(ctx);
            Ok(())
        }
        Command::Logout { wipe } => {
            ctx.client.logout(wipe).await?;
            if wipe {
                println!("Сессия завершена, локальные данные и E2E-состояние удалены.");
            } else {
                println!("Сессия завершена, локальный токен удалён.");
            }
            Ok(())
        }
        Command::Status => status(ctx).await,
        Command::Me => {
            print_account(ctx).await?;
            print_session(ctx);
            Ok(())
        }
        Command::Devices { chat } => {
            let devices = match chat {
                Some(reference) => {
                    // Устройства всех участников чата: так можно сверить
                    // отпечаток собеседника по независимому каналу.
                    let chat = resolve_chat(ctx, &reference).await?;
                    let members = ctx.storage.chat_members(&chat.chat_id).await?;
                    let user_ids: Vec<String> = members
                        .iter()
                        .map(|member| member.user_id.clone())
                        .collect();
                    let _ = ctx.client.refresh_devices(&user_ids).await;
                    let mut devices = Vec::new();
                    for member in &members {
                        devices.extend(ctx.storage.list_devices_of(&member.user_id).await?);
                    }
                    devices
                }
                None => ctx.client.devices().await?,
            };
            if devices.is_empty() {
                println!("Устройств нет.");
            }
            for device in &devices {
                println!("{}", format::device_line(device));
            }
            Ok(())
        }
        Command::RevokeDevice { device_id } => {
            ctx.client.revoke_device(&device_id).await?;
            println!("Устройство {device_id} отозвано.");
            Ok(())
        }
        Command::Chats => {
            let chats = ctx.client.list_chats().await?;
            if chats.is_empty() {
                println!("Чатов нет. Создайте личный чат: chat-create <логин>.");
            }
            for chat in &chats {
                println!("{}", format::chat_line(chat));
            }
            Ok(())
        }
        Command::ChatCreate { peer, e2e } => {
            let chat = ctx.client.create_direct_chat(&peer, e2e).await?;
            let mode = if e2e { "E2E (Olm)" } else { "без E2E (dev)" };
            println!("Создан личный чат с {peer} [{mode}]: {}", chat.chat_id);
            println!("{}", format::chat_line(&chat));
            Ok(())
        }
        Command::Send {
            chat,
            text,
            reply_to,
            wait,
        } => {
            send(
                ctx,
                &chat,
                text.join(" "),
                reply_to,
                Duration::from_secs(wait),
            )
            .await
        }
        Command::History {
            chat,
            limit,
            from_server,
            before,
        } => history(ctx, &chat, limit, from_server, before).await,
        Command::Sync => {
            let progress = ctx.client.sync_now().await?;
            print_sync(progress);
            drain_outbox(ctx).await?;
            Ok(())
        }
        Command::Watch => watch(ctx).await,
    }
}

// ---------------------------------------------------------------------------
// Команды
// ---------------------------------------------------------------------------

async fn status(ctx: &Context) -> Result<()> {
    println!("Сервер: {}", ctx.server);
    println!("TLS: {}", tls_label(&ctx.tls));
    println!("Данные: {}", ctx.data_dir.display());
    ctx.client.start().await?;
    let state = ctx
        .client
        .wait_online(Duration::from_secs(3))
        .await
        .unwrap_or_else(|_| ctx.client.connection_state());
    println!("Соединение: {}", format::connection_state(state));
    print_session(ctx);
    print_account(ctx).await?;
    if let Some(device_id) = ctx
        .secrets
        .get(secret_keys::DEVICE_ID)?
        .and_then(|bytes| String::from_utf8(bytes).ok())
    {
        println!("Устройство: {device_id}");
    }
    println!(
        "Курсор синхронизации: {}",
        ctx.storage.get_sync_cursor().await?
    );
    let chats = ctx.storage.list_chats().await?;
    println!("Чатов в кеше: {}", chats.len());
    let (total, due) = ctx.storage.outbox_stats(rchat_proto::now_ms()).await?;
    println!("Очередь отправки: {total} (готовы к отправке: {due})");
    Ok(())
}

/// Ждёт, пока фоновая отправка опустошит outbox (например, после возвращения
/// сети). По истечении тайм-аута честно сообщает остаток.
async fn drain_outbox(ctx: &Context) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let (total, due) = ctx.storage.outbox_stats(rchat_proto::now_ms()).await?;
        if total == 0 {
            println!("Очередь отправки пуста.");
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            println!("В очереди осталось сообщений: {total} (готовы к отправке: {due}).");
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn send(
    ctx: &Context,
    chat_ref: &str,
    text: String,
    reply_to: Option<u64>,
    wait: Duration,
) -> Result<()> {
    if text.trim().is_empty() {
        return Err("пустое сообщение".into());
    }
    let chat = resolve_chat(ctx, chat_ref).await?;
    let local_id = ctx.client.send_text(&chat.chat_id, &text, reply_to).await?;
    println!("Сообщение поставлено в очередь: {local_id}");

    // Ждём подтверждения сервера, чтобы пользователь сразу видел результат.
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let message = ctx.storage.get_message(&local_id).await?;
        match message.and_then(|message| message.status) {
            Some(MessageStatus::Sent) => {
                println!("Отправлено.");
                return Ok(());
            }
            Some(MessageStatus::Failed) => {
                let error = ctx
                    .storage
                    .get_outbox(&local_id)
                    .await?
                    .and_then(|record| record.last_error)
                    .unwrap_or_else(|| "без деталей".to_string());
                return Err(format!("отправка не удалась: {error}").into());
            }
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            println!(
                "Сообщение остаётся в очереди: оно уйдёт автоматически при подключении (см. `status`, `watch`)."
            );
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn history(
    ctx: &Context,
    chat_ref: &str,
    limit: u32,
    from_server: bool,
    before: Option<u64>,
) -> Result<()> {
    let chat = resolve_chat(ctx, chat_ref).await?;
    let messages = if from_server {
        ctx.client
            .fetch_history(&chat.chat_id, before, limit)
            .await?
    } else {
        ctx.client
            .list_messages(&chat.chat_id, before, limit)
            .await?
    };
    if messages.is_empty() {
        println!("Сообщений нет.");
        return Ok(());
    }
    // Хранилище отдаёт новые первыми; в терминале читаем историю сверху вниз.
    for message in messages.iter().rev() {
        println!("{}", format::message_line(message));
    }
    Ok(())
}

async fn watch(ctx: &Context) -> Result<()> {
    let mut subscription = ctx.client.subscribe();
    ctx.client.start().await?;
    match ctx.client.wait_online(Duration::from_secs(5)).await {
        Ok(state) => println!(
            "Слушаю события ({}). Ctrl+C — выход.",
            format::connection_state(state)
        ),
        Err(error) => println!(
            "Пока нет соединения ({error}); события появятся после подключения. Ctrl+C — выход."
        ),
    }
    // Показываем чаты из кеша и остаток очереди, чтобы контекст был перед глазами.
    for chat in ctx.storage.list_chats().await? {
        println!("[чат] {}", format::chat_line(&chat));
    }
    for record in ctx.storage.due_outbox(rchat_proto::now_ms(), 100).await? {
        println!(
            "[очередь] {}: {}",
            record.local_id,
            format::outbox_state(record.state)
        );
    }
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("Остановка.");
                return Ok(());
            }
            event = subscription.next() => match event {
                Ok(event) => print_event(&event),
                Err(EventError::Lagged(missed)) => println!(
                    "[события] пропущено {missed}; состояние в хранилище актуально"
                ),
                Err(EventError::Closed) => {
                    println!("Поток событий закрыт.");
                    return Ok(());
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Помощники вывода
// ---------------------------------------------------------------------------

fn print_event(event: &UiEvent) {
    match event {
        UiEvent::ConnectionStateChanged(state) => {
            println!("[связь] {}", format::connection_state(*state));
        }
        UiEvent::SessionStarted { username, .. } => println!("[сессия] вход выполнен: {username}"),
        UiEvent::SessionEnded { reason } => println!("[сессия] завершена ({reason})"),
        UiEvent::MessageReceived(message) => {
            println!("[сообщение] {}", format::message_line(message));
        }
        UiEvent::MessageUpdated(message) => {
            println!("[изменено] {}", format::message_line(message))
        }
        UiEvent::OutboxChanged {
            local_id,
            state,
            error,
        } => {
            let suffix = error
                .as_deref()
                .map(|error| format!(" ({error})"))
                .unwrap_or_default();
            println!(
                "[очередь] {local_id}: {}{suffix}",
                format::outbox_state(*state)
            );
        }
        UiEvent::ChatUpserted(chat) => println!("[чат] {}", format::chat_line(chat)),
        UiEvent::ReceiptUpdated(receipt) => {
            println!("[доставка] {}", format::receipt_line(receipt))
        }
        UiEvent::PresenceChanged { user_id, online } => {
            println!(
                "[presence] {user_id}: {}",
                if *online {
                    "в сети"
                } else {
                    "не в сети"
                }
            );
        }
        UiEvent::SyncProgress(progress) => print_sync(*progress),
        UiEvent::ResyncPerformed { up_to_seq } => {
            println!("[resync] состояние пересобрано до seq {up_to_seq}");
        }
        UiEvent::E2eDecryptFailed {
            chat_id,
            server_id,
            device_id,
        } => println!(
            "[E2E] не удалось расшифровать #{server_id} в {chat_id} (устройство {device_id})"
        ),
        UiEvent::Warning(text) => println!("[предупреждение] {text}"),
    }
}

fn print_sync(progress: SyncProgress) {
    println!(
        "[sync] seq {}→{}, применено {}, пропущено {}{}",
        progress.from_seq,
        progress.to_seq,
        progress.events_applied,
        progress.events_skipped,
        if progress.resync_performed {
            ", выполнен resync"
        } else {
            ""
        }
    );
}

fn print_session(ctx: &Context) {
    match ctx.client.session() {
        Some((user_id, username, display_name)) => {
            println!("Сессия: {display_name} ({username}, {user_id}) — активна")
        }
        None => println!("Сессия: не активна (выполните register/login)"),
    }
}

async fn print_account(ctx: &Context) -> Result<()> {
    match ctx.storage.load_account().await? {
        Some(account) => println!(
            "Профиль: {} ({}, устройство {})",
            account.display_name, account.username, account.device_id
        ),
        None => println!("Профиль: не сохранён (выполните register/login)"),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Разбор чата, пароля и конфигурации
// ---------------------------------------------------------------------------

/// Находит чат по `chat_id` или по логину собеседника в личном чате.
async fn resolve_chat(ctx: &Context, reference: &str) -> Result<ChatRecord> {
    if let Some(chat) = ctx.storage.get_chat(reference).await? {
        return Ok(chat);
    }
    let mut matches = Vec::new();
    for chat in ctx.storage.list_chats().await? {
        let Some(peer) = chat.peer_user_id.clone() else {
            continue;
        };
        if peer == reference {
            matches.push(chat);
            continue;
        }
        if let Some(user) = ctx.storage.get_user(&peer).await? {
            if user.username.eq_ignore_ascii_case(reference) {
                matches.push(chat);
            }
        }
    }
    match matches.len() {
        0 => Err(format!(
            "чат {reference:?} не найден: ни chat_id, ни личного чата с таким логином (список — `chats`)"
        )
        .into()),
        1 => Ok(matches.remove(0)),
        _ => Err(format!(
            "логину {reference:?} соответствует несколько чатов: укажите chat_id"
        )
        .into()),
    }
}

/// Читает пароль: stdin (`--password-stdin`), переменная окружения
/// `RCHAT_PASSWORD` или интерактивный запрос без эха.
fn read_password(args: &PasswordArgs, confirm: bool) -> Result<Zeroizing<String>> {
    if args.password_stdin {
        let mut line = String::new();
        std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line)
            .map_err(|error| format!("чтение пароля из stdin: {error}"))?;
        let password = line.trim_end_matches(['\n', '\r']).to_string();
        if password.is_empty() {
            return Err("пустой пароль из stdin".into());
        }
        return Ok(Zeroizing::new(password));
    }
    if let Ok(value) = std::env::var("RCHAT_PASSWORD") {
        if !value.is_empty() {
            return Ok(Zeroizing::new(value));
        }
    }
    let password = Zeroizing::new(
        rpassword::prompt_password("Пароль: ")
            .map_err(|error| format!("чтение пароля: {error}"))?,
    );
    if password.is_empty() {
        return Err("пустой пароль".into());
    }
    if confirm {
        let again = Zeroizing::new(
            rpassword::prompt_password("Повторите пароль: ")
                .map_err(|error| format!("чтение пароля: {error}"))?,
        );
        if *again != *password {
            return Err("пароли не совпадают".into());
        }
    }
    Ok(password)
}

fn resolve_tls(cli: &Cli) -> Result<TlsTrust> {
    Ok(match (&cli.tls_fingerprint, cli.tls_insecure) {
        (Some(hex), _) => TlsTrust::fingerprint_hex(hex)?,
        (None, true) => TlsTrust::Insecure,
        (None, false) => TlsTrust::SystemRoots,
    })
}

fn tls_label(trust: &TlsTrust) -> String {
    match trust {
        TlsTrust::SystemRoots => "системные корневые сертификаты".to_string(),
        TlsTrust::Insecure => "проверка отключена (небезопасно, только локально)".to_string(),
        TlsTrust::Fingerprint(bytes) => {
            let hex = bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(":");
            format!("пиннинг отпечатка {hex}")
        }
    }
}

fn resolve_data_dir(cli: &Cli) -> PathBuf {
    if let Some(dir) = &cli.data_dir {
        return dir.clone();
    }
    if let Ok(dir) = std::env::var("RCHAT_DATA_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    directories::ProjectDirs::from("invalid", "rchat", "rchat")
        .map(|dirs| dirs.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("rchat-data"))
}

fn resolve_device_name(cli: &Cli) -> String {
    if let Some(name) = &cli.device_name {
        return name.clone();
    }
    if let Ok(hostname) = std::env::var("HOSTNAME") {
        if !hostname.is_empty() {
            return hostname;
        }
    }
    "cli-device".to_string()
}

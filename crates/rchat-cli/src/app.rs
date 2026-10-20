//! Composition root CLI: собирает клиентское ядро с конкретными транспортом,
//! хранилищем и хранилищем секретов, и реализует команды.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rchat_client_storage_sqlite::{ClientSqliteStorage, EncryptedFileSecretStore};
use rchat_core::client::{Client, ClientBuilder};
use rchat_core::config::ClientConfig;
use rchat_core::events::{EventError, EventSubscription, UiEvent};
use rchat_core::traits::{secret_keys, SecretStore, Storage};
use rchat_core::types::{ChatMode, ChatRecord, MessageStatus, SyncProgress};
use rchat_crypto_provider::VodozemacCryptoProvider;
use rchat_transport_ws::{TlsTrust, WsTransportConfig, WsTransportFactory};
use zeroize::Zeroizing;

use crate::cli::{CallCommand, Cli, Command, GroupCommand, PasswordArgs};
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
    account: String,
    server: String,
    tls: TlsTrust,
}

/// Точка входа приложения: собирает клиент и выполняет команду.
pub async fn run(cli: Cli) -> Result<()> {
    validate_account_name(&cli.account)?;
    let data_root = resolve_data_dir(&cli);

    // Локальные команды не требуют сети, TLS и сборки клиента: их можно
    // выполнять до открытия БД и запуска фонового актора.
    match &cli.command {
        Command::Accounts => return accounts(&data_root, &cli.account).await,
        Command::AccountDelete { name, yes } => {
            return account_delete(&data_root, &cli.account, name, *yes).await;
        }
        _ => {}
    }

    let tls = resolve_tls(&cli)?;
    let data_dir = account_dir(&data_root, &cli.account);
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
        account: cli.account.clone(),
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
        // Обрабатываются в `run` до сборки клиента (локальные операции).
        Command::Accounts | Command::AccountDelete { .. } => {
            unreachable!("локальные команды аккаунтов не доходят до dispatch")
        }
        Command::Status => status(ctx).await,
        Command::Me => {
            println!(
                "Аккаунт: {} (каталог {})",
                ctx.account,
                ctx.data_dir.display()
            );
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
        Command::ChatCreate { peer, mode } => {
            let mode = resolve_chat_mode(&ctx.client, mode.as_deref())?;
            let chat = ctx.client.create_direct_chat(&peer, mode).await?;
            println!(
                "Создан личный чат с {peer} [{}]: {}",
                mode_label(mode),
                chat.chat_id
            );
            println!("{}", format::chat_line(&chat));
            Ok(())
        }
        Command::GroupCreate {
            name,
            members,
            mode,
        } => {
            let mode = resolve_chat_mode(&ctx.client, mode.as_deref())?;
            let chat = ctx.client.create_group_chat(&name, &members, mode).await?;
            println!(
                "Создан групповой чат [{}]: {}",
                mode_label(mode),
                chat.chat_id
            );
            println!("{}", format::chat_line(&chat));
            if members.is_empty() {
                println!("Участников пока нет: пригласите командой `group invite`.");
            }
            Ok(())
        }
        Command::Group { command } => group(ctx, command).await,
        Command::Call { command } => call(ctx, command).await,
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
        Command::Edit {
            chat,
            server_id,
            text,
        } => edit(ctx, &chat, server_id, text.join(" ")).await,
        Command::Delete { chat, server_id } => delete(ctx, &chat, server_id).await,
        Command::Read { chat, up_to } => read(ctx, &chat, up_to).await,
        Command::Receipts { chat } => receipts(ctx, &chat).await,
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
    println!(
        "Данные: {} (аккаунт: {})",
        ctx.data_dir.display(),
        ctx.account
    );
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

/// Операции с групповым чатом: состав, роли, владение.
async fn group(ctx: &Context, command: GroupCommand) -> Result<()> {
    match command {
        GroupCommand::Info { chat } => {
            let chat = resolve_group(ctx, &chat).await?;
            let members = ctx.client.chat_members(&chat.chat_id).await?;
            println!("{}", format::chat_line(&chat));
            if members.is_empty() {
                println!("Состав пуст.");
                return Ok(());
            }
            println!("Участники ({}):", members.len());
            for member in &members {
                println!("{}", format::member_line(member));
            }
            Ok(())
        }
        GroupCommand::Invite { chat, usernames } => {
            let chat = resolve_group(ctx, &chat).await?;
            let updated = ctx.client.invite_to_chat(&chat.chat_id, &usernames).await?;
            let members = ctx.client.chat_members(&updated.chat_id).await?;
            println!(
                "Приглашены: {}. Состав ({}):",
                usernames.join(", "),
                members.len()
            );
            for member in &members {
                println!("{}", format::member_line(member));
            }
            if updated.e2e_mode == rchat_proto::proto::E2eMode::OlmV1 {
                println!(
                    "Ключ E2E приглашённые получат со следующими сообщениями (ранние им недоступны)."
                );
            }
            Ok(())
        }
        GroupCommand::Leave { chat } => {
            let chat = resolve_group(ctx, &chat).await?;
            ctx.client.leave_chat(&chat.chat_id).await?;
            // Событие обновления состава приходит журналом: забираем его,
            // чтобы вывод отражал фактическое состояние.
            let progress = ctx.client.sync_now().await?;
            println!("Вы вышли из группы {} («{}»).", chat.chat_id, chat.title);
            print_sync(progress);
            let updated = ctx.storage.get_chat(&chat.chat_id).await?;
            if let Some(updated) = updated {
                let members = ctx.client.chat_members(&updated.chat_id).await?;
                println!("Состав без вас ({}):", members.len());
                for member in &members {
                    println!("{}", format::member_line(member));
                }
            }
            Ok(())
        }
        GroupCommand::Remove { chat, user } => {
            let chat = resolve_group(ctx, &chat).await?;
            let user_id = resolve_member(ctx, &chat.chat_id, &user).await?;
            let updated = ctx
                .client
                .remove_chat_member(&chat.chat_id, &user_id)
                .await?;
            println!(
                "Участник {user_id} исключён из {} («{}»).",
                updated.chat_id, updated.title
            );
            if updated.e2e_mode == rchat_proto::proto::E2eMode::OlmV1 {
                println!("Его доступ к новым сообщениям закрыт: сессия E2E ротирована.");
            }
            Ok(())
        }
        GroupCommand::Rename { chat, name } => {
            let chat = resolve_group(ctx, &chat).await?;
            let updated = ctx.client.rename_chat(&chat.chat_id, &name).await?;
            println!("Группа переименована: «{}»", updated.title);
            println!("{}", format::chat_line(&updated));
            Ok(())
        }
    }
}

/// Сигнализация звонка: медиа нет, сервер лишь пересылает события.
async fn call(ctx: &Context, command: CallCommand) -> Result<()> {
    match command {
        CallCommand::Invite { chat, wait } => {
            let chat = resolve_chat(ctx, &chat).await?;
            if chat.kind != rchat_proto::proto::ChatKind::Direct {
                return Err(format!(
                    "чат {} — {}; звонки поддерживаются только в личных чатах",
                    chat.chat_id,
                    format::chat_kind(chat.kind)
                )
                .into());
            }
            // Подписка до запроса: иначе быстрый accept мог бы прийти
            // раньше, чем мы начнём слушать.
            let events = wait.then(|| ctx.client.subscribe());
            let call = ctx.client.call_invite(&chat.chat_id).await?;
            println!(
                "Звонок {}: приглашение отправлено в «{}».",
                call.call_id, chat.title
            );
            if !wait {
                println!(
                    "Это только сигнализация (медиа нет). Ответ: call accept {} или call decline {}.\n\
                     Без --wait процесс завершится и звонок будет снят.",
                    call.call_id, call.call_id
                );
                return Ok(());
            }
            println!(
                "Ожидаю ответа (Ctrl+C — завершить звонок). Принять: call accept {}.",
                call.call_id
            );
            wait_for_call(ctx, &call.call_id, events.expect("subscription for --wait")).await
        }
        CallCommand::Accept { call_id } => {
            let call = ctx.client.call_accept(&call_id).await?;
            println!("Звонок принят: {}", format::call_line(&call));
            Ok(())
        }
        CallCommand::Decline { call_id } => {
            let call = ctx.client.call_decline(&call_id).await?;
            println!("Звонок отклонён: {}", format::call_line(&call));
            Ok(())
        }
        CallCommand::End { call_id } => {
            let call = ctx.client.call_end(&call_id).await?;
            println!("Звонок завершён: {}", format::call_line(&call));
            Ok(())
        }
    }
}

/// Ожидает ответа на приглашение, удерживая соединение (сигнализация).
async fn wait_for_call(ctx: &Context, call_id: &str, mut events: EventSubscription) -> Result<()> {
    let mut deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                let _ = ctx.client.call_end(call_id).await;
                println!("Звонок завершён.");
                return Ok(());
            }
            _ = tokio::time::sleep_until(deadline) => {
                let _ = ctx.client.call_end(call_id).await;
                println!("Нет ответа; звонок завершён.");
                return Ok(());
            }
            event = events.next() => match event {
                Ok(UiEvent::CallAccepted(call)) if call.call_id == call_id => {
                    println!("[звонок] принят: {}", format::call_line(&call));
                    println!("Сигнализация установлена (медиа нет). Ctrl+C — завершить.");
                    // Активный звонок не ограничен кольцевым таймаутом.
                    deadline = tokio::time::Instant::now() + Duration::from_secs(4 * 60 * 60);
                }
                Ok(UiEvent::CallDeclined(call)) if call.call_id == call_id => {
                    println!("[звонок] отклонён: {}", format::call_line(&call));
                    return Ok(());
                }
                Ok(UiEvent::CallEnded(call)) if call.call_id == call_id => {
                    println!("[звонок] завершён: {}", format::call_line(&call));
                    return Ok(());
                }
                Ok(_) => {}
                Err(EventError::Lagged(missed)) => {
                    println!("[события] пропущено {missed}; состояние звонка на сервере");
                }
                Err(EventError::Closed) => return Err("поток событий закрыт".into()),
            }
        }
    }
}

/// Находит групповой чат и проверяет, что это действительно группа.
async fn resolve_group(ctx: &Context, reference: &str) -> Result<ChatRecord> {
    let chat = resolve_chat(ctx, reference).await?;
    if chat.kind != rchat_proto::proto::ChatKind::Group {
        return Err(format!(
            "чат {} — не групповой ({}); команды `group` работают только с группами",
            chat.chat_id,
            format::chat_kind(chat.kind)
        )
        .into());
    }
    Ok(chat)
}

/// Находит участника по user_id или логину; возвращает user_id.
async fn resolve_member(ctx: &Context, chat_id: &str, reference: &str) -> Result<String> {
    let members = ctx.client.chat_members(chat_id).await?;
    if let Some(member) = members.iter().find(|member| member.user_id == reference) {
        return Ok(member.user_id.clone());
    }
    let matches: Vec<&rchat_core::types::MemberRecord> = members
        .iter()
        .filter(|member| member.username.eq_ignore_ascii_case(reference))
        .collect();
    match matches.as_slice() {
        [member] => Ok(member.user_id.clone()),
        [] => Err(format!("участник {reference:?} не найден в чате (см. `group info`)").into()),
        _ => Err(format!("логину {reference:?} соответствует несколько участников").into()),
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

/// Правка собственного сообщения. Для E2E-чата сервер получает только новый
/// шифротекст; локальный текст обновляется сразу.
async fn edit(ctx: &Context, chat_ref: &str, server_id: u64, text: String) -> Result<()> {
    if text.trim().is_empty() {
        return Err("пустое сообщение".into());
    }
    let chat = resolve_chat(ctx, chat_ref).await?;
    let message = ctx
        .client
        .edit_text(&chat.chat_id, server_id, &text)
        .await?;
    println!("Сообщение #{server_id} изменено.");
    println!("{}", format::message_line(&message));
    Ok(())
}

/// Удаление собственного сообщения для всех (tombstone; сервер стирает
/// содержимое).
async fn delete(ctx: &Context, chat_ref: &str, server_id: u64) -> Result<()> {
    let chat = resolve_chat(ctx, chat_ref).await?;
    let message = ctx.client.delete_message(&chat.chat_id, server_id).await?;
    println!("Сообщение #{server_id} удалено для всех.");
    println!("{}", format::message_line(&message));
    Ok(())
}

/// Отметка прочтения: `delivered` устройства отправляет ядро автоматически
/// при применении входящих; здесь пользователь подтверждает прочтение.
async fn read(ctx: &Context, chat_ref: &str, up_to: Option<u64>) -> Result<()> {
    let chat = resolve_chat(ctx, chat_ref).await?;
    let up_to = match up_to {
        Some(value) => value,
        None => chat.last_server_id,
    };
    if up_to == 0 {
        println!("В чате пока нет сообщений.");
        return Ok(());
    }
    ctx.client.mark_read(&chat.chat_id, up_to).await?;
    println!("Отмечено прочитанным: до #{up_to}.");
    Ok(())
}

/// Показывает, кто из участников до какого сообщения дочитал (локальный кеш;
/// обновляется событиями журнала — в том числе у устройств, бывших офлайн).
async fn receipts(ctx: &Context, chat_ref: &str) -> Result<()> {
    let chat = resolve_chat(ctx, chat_ref).await?;
    let receipts = ctx.client.receipts(&chat.chat_id).await?;
    if receipts.is_empty() {
        println!("Подтверждений нет.");
        return Ok(());
    }
    for receipt in &receipts {
        println!("{}", format::receipt_line(receipt));
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
            error_code: _,
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
        UiEvent::CallInvited(call) => {
            println!("[звонок] входящий: {}", format::call_line(call));
            println!(
                "          принять: call accept {} / отклонить: call decline {}",
                call.call_id, call.call_id
            );
        }
        UiEvent::CallAccepted(call) => {
            println!("[звонок] принят: {}", format::call_line(call));
        }
        UiEvent::CallDeclined(call) => {
            println!("[звонок] отклонён: {}", format::call_line(call));
        }
        UiEvent::CallEnded(call) => {
            println!("[звонок] завершён: {}", format::call_line(call));
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
        UiEvent::PrivateModeRequested {
            chat_id,
            requester_username,
            requester_display_name,
            target_mode,
            ..
        } => {
            let what = if *target_mode == rchat_proto::E2eMode::Cloud as i32 {
                "отключить приватный режим"
            } else {
                "включить приватный режим (E2E)"
            };
            println!(
                "[приватный режим] {requester_display_name} (@{requester_username}) предлагает {what} в чате {chat_id}"
            );
        }
        UiEvent::PrivateModeCancelled { chat_id } => {
            println!("[приватный режим] запрос в чате {chat_id} отозван")
        }
        UiEvent::PrivateModeResolved { chat_id, accepted } => println!(
            "[приватный режим] запрос в чате {chat_id} {}",
            if *accepted {
                "принят — чат переведён в E2E"
            } else {
                "отклонён"
            }
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

/// Находит чат по `chat_id`, логину собеседника в личном чате или имени
/// группы (если однозначно).
async fn resolve_chat(ctx: &Context, reference: &str) -> Result<ChatRecord> {
    if let Some(chat) = ctx.storage.get_chat(reference).await? {
        return Ok(chat);
    }
    let mut matches = Vec::new();
    for chat in ctx.storage.list_chats().await? {
        if chat.kind == rchat_proto::proto::ChatKind::Group {
            if chat.title.eq_ignore_ascii_case(reference) {
                matches.push(chat);
            }
            continue;
        }
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
            "чат {reference:?} не найден: ни chat_id, ни личного чата по логину, ни группы с таким именем (список — `chats`)"
        )
        .into()),
        1 => Ok(matches.remove(0)),
        _ => Err(format!(
            "{reference:?} соответствует несколько чатов: укажите chat_id"
        )
        .into()),
    }
}

/// Читает пароль: stdin (`--password-stdin`), переменная окружения
/// `RCHAT_PASSWORD` или интерактивный запрос без эха.
/// Режим чата из аргумента команды; без аргумента — режим по умолчанию из
/// настроек клиента (по умолчанию облачный).
fn resolve_chat_mode(client: &Client, mode: Option<&str>) -> Result<ChatMode> {
    let Some(value) = mode else {
        return Ok(client.default_chat_mode());
    };
    ChatMode::parse(value.trim()).ok_or_else(|| {
        format!("неизвестный режим `{value}`: ожидается cloud, private или none").into()
    })
}

/// Человекочитаемое название режима для вывода в терминал.
fn mode_label(mode: ChatMode) -> &'static str {
    match mode {
        ChatMode::Cloud => "облачный",
        ChatMode::Private => "приватный (E2E)",
        ChatMode::NoneDev => "без шифрования (dev)",
    }
}

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

/// Имя профиля по умолчанию: его данные лежат прямо в каталоге данных
/// (совместимость с одиночным аккаунтом).
const DEFAULT_ACCOUNT: &str = "default";
/// Подкаталог для именованных профилей.
const ACCOUNTS_SUBDIR: &str = "accounts";

/// Проверяет имя профиля: ASCII-буквы, цифры, `-` и `_`, до 32 символов.
/// Ограничение одновременно защищает путь от `..` и разделителей.
fn validate_account_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 32 {
        return Err("имя аккаунта: от 1 до 32 символов".into());
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(format!(
            "имя аккаунта {name:?}: допустимы только ASCII-буквы, цифры, '-' и '_'"
        )
        .into());
    }
    Ok(())
}

/// Каталог профиля: `default` — сам корень, остальные —
/// `<корень>/accounts/<имя>`.
fn account_dir(root: &Path, name: &str) -> PathBuf {
    if name == DEFAULT_ACCOUNT {
        root.to_path_buf()
    } else {
        root.join(ACCOUNTS_SUBDIR).join(name)
    }
}

/// Находит существующие профили: `default` (если в корне есть БД) и
/// подкаталоги `accounts/*` с клиентской БД. Сортирует по имени.
fn discover_accounts(root: &Path) -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();
    if root.join("client.sqlite3").is_file() {
        found.push((DEFAULT_ACCOUNT.to_string(), root.to_path_buf()));
    }
    if let Ok(entries) = std::fs::read_dir(root.join(ACCOUNTS_SUBDIR)) {
        for entry in entries.flatten() {
            if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if validate_account_name(&name).is_err() {
                continue;
            }
            let dir = entry.path();
            if dir.join("client.sqlite3").is_file() {
                found.push((name, dir));
            }
        }
    }
    found.sort_by(|left, right| left.0.cmp(&right.0));
    found
}

/// Краткая сводка профиля для `accounts`.
async fn account_summary(dir: &Path) -> Result<String> {
    let storage = ClientSqliteStorage::open(dir.join("client.sqlite3"))?;
    let account = storage.load_account().await?;
    let chats = storage.list_chats().await?;
    Ok(match account {
        Some(account) => format!(
            "{} ({}, устройство {}, чатов: {})",
            account.display_name,
            account.username,
            account.device_id,
            chats.len()
        ),
        None => format!("вход не выполнен, чатов: {}", chats.len()),
    })
}

/// `accounts`: список локальных профилей; текущий помечен `*`.
async fn accounts(root: &Path, active: &str) -> Result<()> {
    let found = discover_accounts(root);
    if found.is_empty() {
        println!(
            "Локальных аккаунтов нет ({}).\nСоздайте профиль: rchat-cli --account <имя> register <логин>",
            root.display()
        );
        return Ok(());
    }
    println!("Аккаунты в {} (* — текущий):", root.display());
    for (name, dir) in found {
        let marker = if name == active { "*" } else { " " };
        match account_summary(&dir).await {
            Ok(summary) => println!("{marker} {name} — {summary}; {}", dir.display()),
            Err(error) => println!("{marker} {name} — ошибка чтения: {error}"),
        }
    }
    Ok(())
}

/// `account-delete`: удаляет каталог именованного профиля. Локальная
/// операция: серверная сессия не отзывается (для этого — `logout`).
async fn account_delete(root: &Path, active: &str, name: &str, yes: bool) -> Result<()> {
    validate_account_name(name)?;
    if name == DEFAULT_ACCOUNT {
        return Err(
            "профиль default — это сам каталог данных; удалите его вручную, когда в нём нет нужных профилей".into(),
        );
    }
    if name == active {
        return Err(format!(
            "профиль {name} выбран текущим (--account); удалите его из другого профиля"
        )
        .into());
    }
    let dir = account_dir(root, name);
    let metadata = match std::fs::symlink_metadata(&dir) {
        Ok(metadata) => metadata,
        Err(_) => {
            return Err(format!("профиль {name} не найден: {}", dir.display()).into());
        }
    };
    if !metadata.is_dir() {
        return Err(format!("{} — не каталог профиля", dir.display()).into());
    }
    if !dir.join("client.sqlite3").is_file() {
        return Err(format!("{} не похож на профиль rchat", dir.display()).into());
    }
    if !yes {
        {
            use std::io::Write as _;
            print!(
                "Удалить профиль {name} ({})? Серверная сессия не отзывается. [y/N] ",
                dir.display()
            );
            std::io::stdout().flush()?;
        }
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .map_err(|error| format!("чтение ответа: {error}"))?;
        if !matches!(
            answer.trim().to_ascii_lowercase().as_str(),
            "y" | "yes" | "д" | "да"
        ) {
            println!("Отменено.");
            return Ok(());
        }
    }
    std::fs::remove_dir_all(&dir)
        .map_err(|error| format!("удаление {}: {error}", dir.display()))?;
    println!("Профиль {name} удалён: {}", dir.display());
    Ok(())
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

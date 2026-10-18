# rchat

Учебно-рабочий мессенджер на Rust: сервер + клиентское ядро + терминальный
клиент (CLI). Сквозное шифрование личных чатов — Olm (библиотека
[vodozemac], своя криптография не пишется). Запускается локально без Docker и
внешних сервисов: всё состояние — SQLite и файлы в каталоге данных.

Реализация следует спецификации `rchat_technical_spec_v2.md` и завершена:
этапы 1–8, включая testkit, сквозные сценарии, документацию и CI.

[vodozemac]: https://github.com/matrix-org/vodozemac

## Состав репозитория

| Крейт | Что это |
| --- | --- |
| `rchat-proto` | protobuf-схема, конверт `Envelope`, версии и лимиты протокола |
| `rchat-core` | ядро клиента: актор соединения, синхронизация, outbox, контракты хранилищ/транспорта/крипты |
| `rchat-server` | сервисный слой сервера (аккаунты, устройства, чаты, журнал sync, rate-limit) и контракт `Repo` |
| `rchat-server-storage-sqlite` | SQLite-реализация `Repo`, сетевой слой TLS + WebSocket, бинарник `rchat-server` |
| `rchat-transport-ws` | WebSocket + TLS транспорт клиента (системные корни, пиннинг отпечатка) |
| `rchat-client-storage-sqlite` | клиентское хранилище (SQLite) и шифрованное файловое хранилище секретов |
| `rchat-crypto-provider` | E2E-провайдер Olm на vodozemac |
| `rchat-testkit` | тестовые инструменты: `TestServer`, `MockStorage`, транспорт с управляемыми помехами |
| `rchat-e2e-tests` | сквозные сценарии (реальный Olm и «мок-стек») |
| `rchat-cli` | CLI — composition root: собирает ядро с конкретными адаптерами |

Направление зависимостей — только «вниз»: ядро не знает об адаптерах.
Правило закреплено тестом `crates/rchat-core/tests/dependency_rules.rs`.
Подробности — в `docs/architecture.md`.

## Требования

- Rust stable (версия и компоненты закреплены в `rust-toolchain.toml`);
- Linux или macOS (права `0600` на файл секретов; на других ОС это
  ограничение честно отражено в `docs/threat-model.md`).

## Быстрый старт

```bash
# Сборка сервера и CLI.
cargo build --bin rchat-server --bin rchat-cli

# 1. Сервер с dev-TLS: самоподписанный сертификат, SHA-256 отпечаток
#    печатается в лог при запуске.
./target/debug/rchat-server --db /tmp/rchat/rchat.sqlite3 --listen 127.0.0.1:8443

# 2. Параметры подключения можно задать переменными окружения: они читаются
#    всеми командами CLI.
export RCHAT_SERVER=wss://localhost:8443
export RCHAT_TLS_FINGERPRINT=<sha256-отпечаток из лога>   # ':' допустимы

# 3. Регистрация двух пользователей (пароль вводится скрытно).
./target/debug/rchat-cli --data-dir /tmp/rchat/alice register alice
./target/debug/rchat-cli --data-dir /tmp/rchat/bob register bob

# 4. E2E-чат (выводит chat_id), отправка, синхронизация, история.
./target/debug/rchat-cli --data-dir /tmp/rchat/alice chat-create bob --e2e
./target/debug/rchat-cli --data-dir /tmp/rchat/alice send <chat_id> Привет
./target/debug/rchat-cli --data-dir /tmp/rchat/bob sync
./target/debug/rchat-cli --data-dir /tmp/rchat/bob history <chat_id>

# 5. Живой режим: события до Ctrl+C (сообщения, presence, outbox).
./target/debug/rchat-cli --data-dir /tmp/rchat/bob watch
```

Сервер можно перезапускать с тем же `--db`: сессии, история и журнал
синхронизации сохраняются; клиент при следующем запуске догружает
пропущенные события.

### Локальный режим без TLS

Для локальных экспериментов (только loopback!) TLS можно отключить и
разрешить dev-чаты без E2E. Такой режим **не является защищённым**:

```bash
./target/debug/rchat-server --db /tmp/rchat/dev.sqlite3 --listen 127.0.0.1:8443 \
    --plaintext-insecure --allow-dev-plaintext-chats

./target/debug/rchat-cli --server ws://127.0.0.1:8443 --data-dir /tmp/rchat/alice register alice
./target/debug/rchat-cli --data-dir /tmp/rchat/alice chat-create bob            # dev-чат, без E2E
./target/debug/rchat-cli --data-dir /tmp/rchat/alice send bob Привет
```

В скриптах пароль можно передать через `--password-stdin` (одна строка в
stdin) или переменной окружения `RCHAT_PASSWORD`.

### Офлайн-режим

Отправленное без сервера сообщение не теряется: оно попадает в локальную
очередь (outbox) и уходит автоматически при следующем подключении. Проверка:

```bash
# Сервер выключен.
./target/debug/rchat-cli --data-dir /tmp/rchat/alice \
    --server ws://127.0.0.1:8443 send bob "уйдёт, когда поднимется сервер" --wait 1
# ...запускаем сервер. Команда `sync` дожидается соединения и выгружает
# очередь (то же делает `watch`, удерживая соединение).
./target/debug/rchat-cli --data-dir /tmp/rchat/alice sync
./target/debug/rchat-cli --data-dir /tmp/rchat/alice status
```

## Команды CLI

`register`, `login`, `logout`, `status`, `me`, `devices`
(`devices --chat <логин>` — устройства участников для сверки отпечатков),
`revoke-device`, `chats`, `chat-create <логин> [--e2e]`,
`send <чат> <текст>... [--reply-to <server_id>] [--wait N]`,
`history <чат> [--from-server] [--before <server_id>] [--limit N]`,
`edit <чат> <server_id> <текст>`, `delete <чат> <server_id>`,
`read <чат> [<server_id>]`, `receipts <чат>`, `sync`, `watch`.

Полная справка — `rchat-cli --help` и `rchat-cli <команда> --help`.

## Проверки качества

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --bin rchat-server --bin rchat-cli
```

Те же команды выполняет CI (`.github/workflows/ci.yml`, GitHub Actions без
Docker). Что именно покрывают тесты и как пользоваться testkit — в
`docs/testing.md`.

## Документация

- `docs/architecture.md` — структура, решения по крейтам, серверу, ядру, E2E;
- `docs/protocol.md` — протокол обмена (кадры, методы, синхронизация, версии);
- `docs/testing.md` — слои тестов, testkit, сценарии сбоев, ручной smoke;
- `docs/threat-model.md` — модель угроз, гарантии и ограничения;
- `docs/extension-guide.md` — как расширять: транспорт, хранилище, UI,
  протокол, криптопровайдер.

## Известные ограничения

- E2E (Olm) — только личные чаты; защищается содержимое, не метаданные;
- распространение ключей устройств — trust on first use: подмена
  обнаруживается только сверкой отпечатков по независимому каналу;
- presence — best-effort и транзиентен (в памяти одного процесса);
- удаление — только «для всех» (tombstone); уже расшифрованные копии не
  отзываются;
- сервер — один процесс: реестр соединений и rate-limit в памяти;
- сессии — по одной на устройство, без зависания на нескольких серверах.

Полный список — `docs/architecture.md` §11 и `docs/threat-model.md`.

## Лицензия

Apache-2.0.

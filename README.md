# rchat

Учебно-рабочий мессенджер на Rust: сервер + клиентское ядро + терминальный
клиент (CLI). Сквозное шифрование — Olm для личных чатов и Megolm для
групповых (библиотека [vodozemac], своя криптография не пишется). Запускается
локально без Docker и внешних сервисов: всё состояние — SQLite и файлы в
каталоге данных.

Реализация следует спецификации `rchat_technical_spec_v2.md` и завершена:
этапы 1–8, включая testkit, сквозные сценарии, документацию и CI.

Сверх первого релиза добавлены **групповые чаты**: немедленное членство,
роли (владелец/участник) и сквозное шифрование на Megolm (ключ сессии
рассылается персональными Olm-конвертами, ротация — при исключении участника
и отзыве устройства). Инварианты и правила — в `docs/protocol.md` §4,
устройство — в `docs/architecture.md` §7.5, оговорки — в
`docs/threat-model.md`. Также добавлены мультиаккаунтные профили CLI,
сигнализация звонков и встраиваемый **C-ABI/JSON-интерфейс** (`rchat-ffi`,
стиль tdjson); готовых языковых биндингов пока нет.

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
| `rchat-crypto-provider` | E2E-провайдер Olm (личные чаты) и Megolm (группы) на vodozemac |
| `rchat-testkit` | тестовые инструменты: `TestServer`, `MockStorage`, транспорт с управляемыми помехами |
| `rchat-e2e-tests` | сквозные сценарии (реальный Olm/Megolm и «мок-стек») |
| `rchat-cli` | CLI — composition root: собирает ядро с конкретными адаптерами |
| `rchat-ffi` | C-ABI/JSON-интерфейс для встраивания (cdylib/staticlib), один handle — один профиль |

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

# 6. Групповой чат: создание (E2E по умолчанию), состав, приглашение.
./target/debug/rchat-cli --data-dir /tmp/rchat/alice group-create Проект --members bob
./target/debug/rchat-cli --data-dir /tmp/rchat/alice send <chat_id> Всем привет
./target/debug/rchat-cli --data-dir /tmp/rchat/bob sync
./target/debug/rchat-cli --data-dir /tmp/rchat/bob group info <chat_id>

# 7. Звонок — только сигнализация, без медиа. Bob слушает события в watch;
#    Alice приглашает и удерживает соединение флагом --wait (в другом
#    терминале). call_id печатается в обоих окнах.
./target/debug/rchat-cli --data-dir /tmp/rchat/bob watch
./target/debug/rchat-cli --data-dir /tmp/rchat/alice call invite <chat_id> --wait
# В watch у Bob: call accept <call_id> или call decline <call_id>
# (удобно вторым устройством/профилем); call end <call_id> — любой участник.
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

### Несколько аккаунтов

Флаг `--account` (или `RCHAT_ACCOUNT`) выбирает локальный профиль.
`default` — сам каталог данных (прежнее поведение), именованные профили
лежат в `<data-dir>/accounts/<имя>` и не делят ни БД, ни секреты, ни
E2E-состояние:

```bash
# Профиль work в общем каталоге данных.
./target/debug/rchat-cli --data-dir /tmp/rchat --account work register alice
./target/debug/rchat-cli --data-dir /tmp/rchat --account work chat-create bob
./target/debug/rchat-cli --data-dir /tmp/rchat --account work accounts
# * work — alice (...), чатов: 1; /tmp/rchat/accounts/work
# (профиль default не создан — в корне нет client.sqlite3)

# Другой аккаунт рядом, не смешивая состояние.
RCHAT_ACCOUNT=home ./target/debug/rchat-cli --data-dir /tmp/rchat register bob

# Удалить только локальные данные профиля. Серверная сессия не отзывается:
# сначала `--account work logout` (или `logout --wipe`).
./target/debug/rchat-cli --data-dir /tmp/rchat account-delete work --yes
```

Профиль `default` удалить командой нельзя — это сам каталог данных
(в нём могут лежать другие профили).

Несколько аккаунтов можно держать и в одном процессе: у `rchat-core` нет
глобального состояния, каждый `Client` собирается со своими хранилищем,
секретами и транспортом. Это проверяет
`crates/rchat-e2e-tests/tests/multi_account.rs`: три аккаунта одновременно
работают в общем tokio-runtime. CLI запускает один профиль на процесс;
параллельные аккаунты — отдельные процессы, файлы профилей не пересекаются.

## Команды CLI

`register`, `login`, `logout`, `accounts`, `account-delete <имя> [--yes]`,
`status`, `me`, `devices`
(`devices --chat <логин>` — устройства участников для сверки отпечатков),
`revoke-device`, `chats`, `chat-create <логин> [--e2e]`,
`group-create <имя> [--members a,b]` (E2E по умолчанию; `--no-e2e` — только
с dev-сервером), `group info|invite|leave|remove|rename <чат|группа> ...`
(участник приглашает; исключает и переименовывает владелец),
`send <чат> <текст>... [--reply-to <server_id>] [--wait N]`,
`history <чат> [--from-server] [--before <server_id>] [--limit N]`,
`edit <чат> <server_id> <текст>`, `delete <чат> <server_id>`,
`read <чат> [<server_id>]`, `receipts <чат>`, `sync`, `watch`,
`call invite <чат> [--wait]`, `call accept|decline|end <call_id>`
(только личные чаты, без медиа; `--wait` удерживает соединение и ждёт
ответа — без него процесс завершается и звонок снимается).
Группу можно указывать по `chat_id` или по имени (если оно однозначно).

Полная справка — `rchat-cli --help` и `rchat-cli <команда> --help`.

## Встраивание: C-ABI и JSON

```bash
cargo build -p rchat-ffi --release
```

Библиотека (`librchat_ffi.so`/`.dylib`/`.dll` и staticlib) и заголовок
`crates/rchat-ffi/include/rchat.h` дают tdjson-подобный интерфейс:

```c
#include <stdio.h>
#include "rchat.h"

RchatHandle *h = rchat_create(
    "{\"data_dir\":\"/tmp/rchat-ffi\","
    "\"server\":\"wss://chat.example\","
    "\"auto_start\":false}");
char *response = rchat_send_json(h, "{\"@type\":\"ping\"}");
puts(response);
rchat_free_string(response);
rchat_destroy(h);
```

`rchat_send_json` синхронно выполняет JSON-запрос, `rchat_receive_json`
забирает события (сообщения, receipts, звонки, статус соединения) с
тайм-аутом. Один handle — один независимый профиль; несколько handle могут
работать в одном процессе. Полная схема команд, событий, ошибок и
ограничения — `docs/ffi.md`; минимальный пример —
`crates/rchat-ffi/examples/ffi_smoke.c`.

## Проверки качества

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --bin rchat-server --bin rchat-cli
cargo build -p rchat-ffi
```

Те же команды выполняет CI (`.github/workflows/ci.yml`, GitHub Actions без
Docker). Что именно покрывают тесты и как пользоваться testkit — в
`docs/testing.md`.

## Документация

- `docs/architecture.md` — структура, решения по крейтам, серверу, ядру, E2E;
- `docs/protocol.md` — протокол обмена (кадры, методы, синхронизация, версии);
- `docs/group-e2e.md` — отдельная спецификация группового E2E (Megolm);
- `docs/testing.md` — слои тестов, testkit, сценарии сбоев, ручной smoke;
- `docs/ffi.md` — C-ABI/JSON: конфигурация, команды, события, ошибки, сборка;
- `docs/threat-model.md` — модель угроз, гарантии и ограничения;
- `docs/extension-guide.md` — как расширять: транспорт, хранилище, UI,
  протокол, криптопровайдер.

## Известные ограничения

- E2E: Olm для личных чатов, Megolm для групп; защищается содержимое, не
  метаданные. Приглашённый не читает сообщения до приглашения; исключённый
  сохраняет уже расшифрованную историю (ротация закрывает только будущее);
- распространение ключей устройств — trust on first use: подмена
  обнаруживается только сверкой отпечатков по независимому каналу;
- presence — best-effort и транзиентен (в памяти одного процесса);
- звонки — только сигнализация (invite/accept/decline/end), без медиа и
  WebRTC; только личные чаты, состояние в памяти сервера, callee должен быть
  в сети, события transient и могут теряться при офлайне;
- удаление — только «для всех» (tombstone); уже расшифрованные копии не
  отзываются;
- сервер — один процесс: реестр соединений и rate-limit в памяти;
- сессии — по одной на устройство, без зависания на нескольких серверах;
- C-ABI/JSON (`rchat-ffi`) поставляется без готовых языковых биндингов;
  API-схема версии 1 может меняться до стабилизации 1.0.

Полный список — `docs/architecture.md` §11 и `docs/threat-model.md`.

## Лицензия

Apache-2.0.

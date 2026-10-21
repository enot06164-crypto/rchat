# rchat

[![CI](https://github.com/enot06164-crypto/rchat/actions/workflows/ci.yml/badge.svg)](https://github.com/enot06164-crypto/rchat/actions/workflows/ci.yml)
[![license: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Учебно-рабочий мессенджер на Rust: сервер + клиентское ядро + терминальный
клиент (CLI). Два режима чатов, как в Telegram: **облачный** (по умолчанию —
сервер знает ключ чата, умеет искать по истории и отдаёт её новому устройству)
и **приватный** (сквозное шифрование: Olm для личных чатов и Megolm для
групповых, библиотека [vodozemac], своя криптография не пишется). Запускается
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
сигнализация звонков, встраиваемый **C-ABI/JSON-интерфейс** (`rchat-ffi`,
стиль tdjson), детерминированные метаданные аватаров (инициалы и цвет в
JSON), официальный **Python SDK** (`bindings/python`, только стандартная
библиотека), экспорт **JSON Schema** (`schema/rchat.schema.json`) для
генерации типизированных клиентов и **веб-клиент** (этап H,
`apps/web` на SolidJS; браузер работает с ядром через локальный
WebSocket-шлюз `rchat.gateway`, протокол FFI не меняется).

[vodozemac]: https://github.com/matrix-org/vodozemac

## Режимы чатов

| | Приватный (`private`) | Облачный (`cloud`, по умолчанию) |
| --- | --- | --- |
| Шифрование | Olm/Megolm, ключ на устройство | ключ чата (его знает сервер) |
| Сервер видит текст | нет | да (в этом и смысл режима) |
| Поиск по истории | только на устройстве | на сервере, по всем чатам |
| Новое устройство аккаунта | без истории | сразу вся история |
| Файл на диске сервера | шифротекст (ключ в сообщении) | шифротекст под ключом аккаунта, сервер отдаёт открытое содержимое |
| Одинаковый файл у разных людей | хранится у каждого | **хранится один раз** (дедупликация по SHA-256) |

Режим по умолчанию меняется в настройках клиента (веб: меню профиля →
«Настройки»; FFI: `set_default_chat_mode`; CLI: `chat-create --mode`), и
применяется к новым чатам — уже созданные чаты сохраняют свой режим. Подробно
— `docs/architecture.md` §7.6.

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
| `rchat-ffi` | C-ABI/JSON-интерфейс для встраивания (cdylib/staticlib), один handle — один профиль; экспорт `schema/rchat.schema.json` |
| `apps/web` | веб-клиент (этап H-Final): SolidJS + TypeScript + Vite в теме Telegram WebK «Amethyst Dark», общается с ядром через WebSocket-шлюз `rchat.gateway` |

Направление зависимостей — только «вниз»: ядро не знает об адаптерах.
Правило закреплено тестом `crates/rchat-core/tests/dependency_rules.rs`.
Подробности — в `docs/architecture.md`.

Официальный Python SDK лежит в `bindings/python`: низкоуровневая обёртка
`rchat.ffi` над C-ABI и высокоуровневый `RChatClient` с фоновым потоком
событий. Сторонние зависимости не нужны — только стандартный `ctypes`.

## Требования

- Rust stable (версия и компоненты закреплены в `rust-toolchain.toml`);
- `protoc` — компилятор protobuf; нужен для кодогенерации схемы протокола
  (`rchat-proto`). На Debian/Ubuntu: `apt-get install protobuf-compiler`,
  на Fedora: `dnf install protobuf-compiler`, на macOS: `brew install protobuf`;
- Linux или macOS (права `0600` на файл секретов; на других ОС это
  ограничение честно отражено в `docs/threat-model.md`);
- Python 3.9+ — только для Python SDK и его тестов; серверу и CLI не нужен.

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

# 4. Чат (выводит chat_id), отправка, синхронизация, история.
#    По умолчанию — облачный режим; приватный — `--mode private`.
./target/debug/rchat-cli --data-dir /tmp/rchat/alice chat-create bob
./target/debug/rchat-cli --data-dir /tmp/rchat/alice send <chat_id> Привет
./target/debug/rchat-cli --data-dir /tmp/rchat/bob sync
./target/debug/rchat-cli --data-dir /tmp/rchat/bob history <chat_id>

# 5. Живой режим: события до Ctrl+C (сообщения, presence, outbox).
./target/debug/rchat-cli --data-dir /tmp/rchat/bob watch

# 6. Групповой чат: создание (`--mode private` — сквозное шифрование),
#    состав, приглашение.
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
./target/debug/rchat-cli --data-dir /tmp/rchat/alice chat-create bob            # облачный чат (по умолчанию)
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
`revoke-device`, `chats`, `chat-create <логин> [--mode cloud|private|none]`,
`group-create <имя> [--members a,b] [--mode ...]` (режим по умолчанию — из
настроек клиента, то есть облачный; `--mode none` — только с dev-сервером),
`group info|invite|leave|remove|rename <чат|группа> ...`
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

Python SDK использует тот же C-ABI:

```python
from rchat import RChatClient

with RChatClient("/tmp/rchat/alice", "wss://chat.example",
                 tls_fingerprint="AA:BB:...") as client:
    client.login("alice", "password")
    chat = client.create_direct_chat("bob", mode="private")
    client.send_message(chat.chat_id, "привет")
    for event in client.events():
        if event.event == "message_received":
            print(event.data.get("body"))
```

Сборка библиотеки, примеры и тесты — в `bindings/python/README.md`.

## Вложения (файлы, фото, видео, голос)

Ядро умеет вложения — **как в Telegram**: файл шифруется клиентом случайным
ключом (XChaCha20-Poly1305) и уходит на сервер **отдельным блобом** (частями, с
докачкой), а в зашифрованном сообщении остаётся только ссылка на блоб и ключ.
Сервер видит шифротекст файла, но не ключ и не содержимое.

```json
{"@type": "send_message", "chat_id": "<chat_id>", "text": "смотри",
 "attachment": {"kind": "photo", "name": "pic.png", "mime": "image/png",
                "size": 20480, "data_base64": "<base64>"}}
```

`get_messages` отдаёт метаданные вложения, содержимое — отдельной командой
`download_attachment` (ядро само скачивает блоб и расшифровывает). Подробности
— `docs/ffi.md` §5.4. Ограничение: 16 МиБ на файл.

Содержимое файлов хранится **на диске сервера** (каталог `blobs` рядом с БД),
в базе — только метаданные (путь, размер, владелец, токен).

Дополнительно: дедупликация загрузок по SHA-256 (файл не заливается повторно),
локальный кэш скачанных файлов (512 МиБ), постер-кадр видео и пики звуковой
волны — отдельными блобами, альбомы (несколько вложений с общим `grouped_id`).

## Веб-клиент (этап H)

Официальный веб-интерфейс как реплика Telegram WebK (тема **Amethyst
Dark**): `apps/web` — SolidJS + TypeScript (strict) + Vite; геометрия,
расположение кнопок и кривые анимаций сверены с исходниками
`TelegramOrg/Telegram-web-k` (взяты числовые параметры и правила раскладки;
код, SVG-пути и обои написаны с нуля). Аватары-инициалы из ядра, группировка
пузырей с хвостиками, реплаи с цитатами, контекстное меню сообщения, правая
колонка «Информация», плавающий FAB-карандаш. Браузер общается с ядром через
локальный **WebSocket-шлюз** (`rchat.gateway` из Python SDK): JSON-запросы
проксируются в `rchat_send_json`, события ядра ретранслируются всем вкладкам;
протокол FFI (`docs/ffi.md`) остаётся неизменным.

```bash
cargo build --bin rchat-server

# 1) сервер разработки
cargo run --bin rchat-server -- \
    --db /tmp/rchat-web/server.sqlite3 --listen 127.0.0.1:9000 \
    --plaintext-insecure --allow-dev-plaintext-chats

# 2) шлюз (нужен пакет websockets: pip/uv install websockets).
#    --data-dir — корень профилей (аккаунтов): «default» — сам корень,
#    остальные — в <корень>/accounts/<имя>. В браузере аккаунты
#    переключаются чипами на экране входа и в меню профиля.
PYTHONPATH=bindings/python python3 -m rchat.gateway \
    --data-dir /tmp/rchat-web --server ws://127.0.0.1:9000 --port 8080

# 3) веб-клиент (dev-сервер проксирует /ws на шлюз)
cd apps/web && npm install && npm run dev
```

`npm run build` — строгая проверка TypeScript + сборка в `apps/web/dist`;
`npm run preview` отдаёт её с тем же прокси `/ws`. Детали, ограничения v1 и
протокольный смок шлюза — в `apps/web/README.md`.

## Проверки качества

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --bin rchat-server --bin rchat-cli
cargo build -p rchat-ffi
cargo run -p rchat-ffi --bin export_schema -- --check
python3 -m unittest discover -s bindings/python/tests
cd apps/web && npm test          # vitest: тесты стора (дедуп сообщений и др.)
```

Те же команды выполняет CI (`.github/workflows/ci.yml`, GitHub Actions без
Docker). Что именно покрывают тесты и как пользоваться testkit — в
`docs/testing.md`.

## Документация

- `docs/architecture.md` — структура, решения по крейтам, серверу, ядру, E2E;
- `docs/protocol.md` — протокол обмена (кадры, методы, синхронизация, версии);
- `docs/group-e2e.md` — отдельная спецификация группового E2E (Megolm);
- `docs/testing.md` — слои тестов, testkit, сценарии сбоев, ручной smoke;
- `docs/ffi.md` — C-ABI/JSON: конфигурация, команды, события, аватары, ошибки, сборка;
- `schema/rchat.schema.json` — JSON Schema всех команд, ответов и событий;
- `bindings/python/README.md` — Python SDK: установка, примеры, тесты;
- `apps/web/README.md` — веб-клиент: запуск (сервер + шлюз + Vite), сборка, ограничения;
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
- C-ABI/JSON (`rchat-ffi`) и Python SDK (`bindings/python`) используют
  API-схему версии 1; до стабилизации 1.0 она может меняться. Биндингов для
  других языков пока нет.

Полный список — `docs/architecture.md` §11 и `docs/threat-model.md`.

## Лицензия

Apache-2.0 — полный текст в файле [LICENSE](LICENSE).

# Тестирование rchat

Документ описывает слои тестов, инструменты (`rchat-testkit`), сквозные
сценарии сбоев и ручную проверку запуска. Все команды выполняются из корня
workspace.

## 1. Слои тестов

| Слой | Где | Что проверяет |
| --- | --- | --- |
| Юнит-тесты | `src/**` всех крейтов | Кодирование/декодирование конверта и лимиты, валидации, backoff, rate-limit, миграции схемы, форматирование CLI. |
| Правила зависимостей | `crates/rchat-core/tests/dependency_rules.rs` | Манифест ядра не содержит запрещённых крейтов (транспорт, SQL/СУБД, HTTP-фреймворки, CLI-парсеры, криптография, реализации secret-store, `directories`, `rpassword`). |
| Контрактные тесты хранилищ | `crates/rchat-client-storage-sqlite/tests/`, `crates/rchat-server-storage-sqlite/tests/` | Семантика `Storage`/`CryptoStateStore` и `Repo`: идемпотентность вставки, порядок, транзакции, retention/resync. |
| Серверный слой | `crates/rchat-server/tests/`, `crates/rchat-server-storage-sqlite/tests/` | `run_connection` и сервисные операции: handshake, ошибки, чаты, отправка, receipts, покрытие E2E. |
| Сквозные E2E (реальный Olm) | `crates/rchat-e2e-tests/tests/e2e_olm.rs`, `message_edit_delete.rs` | Настоящий сервер (loopback-TCP, без TLS) и настоящие клиенты с **vodozemac**: сессии, прекей-сообщения, мультиустройство, правка/удаление, квитанции, отсутствие plaintext на сервере. |
| Сквозные E2E групп (реальный Megolm) | `crates/rchat-e2e-tests/tests/group_e2e.rs` | Групповой чат: выдача ключа сессии, инкрементальная рассылка, ротация при исключении, недоступность ранних сообщений приглашённому, мультиустройство, запрет dev-групп. |
| Сквозные E2E мультиаккаунта | `crates/rchat-e2e-tests/tests/multi_account.rs` | Три аккаунта в одном процессе: независимые личности и устройства, параллельная отправка, отсутствие утечки чатов между аккаунтами. |
| Серверные сценарии звонков | `crates/rchat-server-storage-sqlite/tests/call_signaling.rs` | Только личные чаты, роли accept/decline/end, busy, offline, transient push-события, неизвестный call_id. |
| Сквозные E2E звонков | `crates/rchat-e2e-tests/tests/call_signaling.rs` | Реальные клиенты: invite → accept → end, decline, busy, запрет групп, доставка событий всем устройствам участника. |
| Сквозные сценарии сбоев («мок-стек») | `crates/rchat-e2e-tests/tests/mock_stack.rs` | Outbox и синхронизация при разрывах/задержках/дублях/битых кадрах; реальный сервер, но канал в памяти и in-memory хранилище из testkit. |
| CLI smoke | `crates/rchat-cli/tests/cli_smoke.rs` | Реальный бинарник `rchat-cli` отдельными процессами: регистрация, чат, отправка, sync/history/status, офлайн-очередь, рестарты сервера, групповой жизненный цикл (`group-*`), профили аккаунтов (`--account`, `accounts`, `account-delete`), звонки (`call invite --wait`, `watch`, accept/decline/end). |
| Тесты testkit | `crates/rchat-testkit/tests/testkit.rs` | Транзакционность `MockStorage` (включая инъекцию сбоя) и порядок применения помех транспорта. |

## 2. Как запускать

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --bin rchat-server --bin rchat-cli
```

Отдельные наборы и подробный вывод:

```bash
cargo test -p rchat-e2e-tests --test mock_stack -- --nocapture
cargo test -p rchat-cli --test cli_smoke
cargo test -p rchat-core dependency_rules
```

Внешние сервисы и Docker не требуются: SQLite собирается в bundled-режиме,
TLS — на `rustls` без системной библиотеки OpenSSL. Тестам не нужны
сетевые порты вне loopback; канальные сценарии работают вообще без сокетов.

Те же команды выполняет CI (`.github/workflows/ci.yml`).

## 3. Testkit (`rchat-testkit`)

Крейт — **только dev-зависимость** тестовых крейтов; в обычную сборку
приложения он не попадает (см. состав зависимостей `rchat-cli` и
`rchat-e2e-tests`).

### 3.1. TestServer

`TestServer` поднимает **настоящий** `rchat_server::Server` + `SqliteRepo`
(никаких mock-серверов), но с тестовыми параметрами: минимальная стоимость
Argon2, разрешённые dev-чаты (тест может запретить).

- `TestServer::start()` / `start_with(|config| ...)` — собственная временная
  БД; `patch` донастраивает `ServerConfig`;
- `TestServer::start_with_db(path, ...)` — существующий файл БД: сессии,
  история и журнал переживают «рестарт сервера» между запусками;
- `TestServer::start_in_memory(...)` — БД и соединения целиком в памяти;
- `port()`, `ws_url()`, `db_path()` — для TCP-клиентов и проверок БД;
- `channel_factory()` — транспорт «каналы в памяти»: `run_connection` без
  сокетов и TLS (быстрые сценарии с mock-транспортом);
- `shutdown().await` — остановка приёма новых соединений.

### 3.2. MockStorage и MockSecretStore

`MockStorage` повторяет семантику SQLite-хранилища клиента, необходимую
тестам ядра:

- `apply_sync_batch` — атомарное применение пачки мутаций и курсора;
  `fail_next_batch()` инжектирует сбой следующей транзакции (курсор не
  двигается, состояние не меняется);
- дедупликация по `local_id`/`server_id`; порядок сообщений как в SQLite
  (новые первыми); монотонные receipts/отзывы устройств; tombstone стирает
  текст; подтверждение отправки удаляет «эхо»-копию и поднимает
  `last_server_id` чата.

`MockSecretStore` — секреты в памяти для клиента без файловой системы.

### 3.3. MockTransportFactory

Оборачивает любую `TransportFactory` (канальную или TCP) и даёт
`FaultHandle`:

| Метод | Что делает |
| --- | --- |
| `drop_next_inbound()` | Следующее чтение — корректное закрытие соединения (эмуляция разрыва). |
| `fail_next_inbound()` / `fail_next_send()` | Ошибка транспорта на следующем чтении/записи. |
| `delay_next_inbound(d)` / `delay_next_send(d)` | Задержка следующего кадра. |
| `duplicate_next_inbound()` | Следующий полученный кадр будет выдан дважды. |
| `inject_inbound(frame)` | Выдать произвольный кадр (битый/чужой/устаревший). |
| `sent_frames()` | Записанные исходящие кадры для проверок. |
| `connections()` | Число соединений (реконнекты видны по росту). |
| `clear()` | Снять неиспользованные помехи. |

Помехи хранятся в очереди и применяются к следующему вызову `recv`/`send`
активного соединения; очередь переживает реконнект. Внутренний транспорт
обязан быть cancel-safe в `recv` (tokio-каналы таковы; канальная фабрика
testkit подходит). Для детерминизма тесты сначала ставят помеху, затем
выполняют операцию, вызывающую нужный кадр.

### 3.4. Фикстуры

`rchat_testkit::fixtures` — билдеры записей для юнит-тестов: `at(secs)`
(отсчёт от `BASE_MS = 1_700_000_000_000`), `account`, `user`, `device`,
`device_info`, `direct_chat`, `member`, `incoming_message`,
`outgoing_message`.

## 4. Политика тестов

- **Mock-криптография не заменяет E2E.** Сценарии, проверяющие шифрование,
  идут только с реальным провайдером (vodozemac) на настоящем сервере.
  Клиент без провайдера (`e2e_enabled = false`) используется лишь в
  dev-чатах — например, в «мок-стеке», где проверяются сетевые инварианты,
  а не крипта.
- **Testkit не в приложении.** Он не подключён ни к `rchat-cli`, ни к
  серверному бинарнику; только `[dev-dependencies]` тестовых крейтов.
- **Событийные ожидания, а не «sleep-ассерты».** Реконнект идёт с backoff и
  джиттером, поэтому тесты ждут состояние циклами с общим лимитом
  (например, 20 с), а не фиксированными паузами.
- **Идемпотентность проверяется повторной доставкой.** Дублируются кадры и
  запросы, а не только «счастливый путь».

## 5. Сценарии сбоев («мок-стек»)

Источник: `crates/rchat-e2e-tests/tests/mock_stack.rs`.

- `outbox_survives_faults_and_reconnect` — разрыв соединения
  (`drop_next_inbound`), реконнект, затем отправка на фоне задержки и повтора
  кадров: сообщение доставляется ровно один раз, outbox очищается, обе
  стороны видят одну копию.
- `duplicate_frames_and_malformed_input_survive` — повтор кадра push не
  рвёт соединение; одиночный битый кадр безопасно пропускается; поток битых
  кадров (более 10 за соединение) закрывает соединение, клиент
  переподключается и после этого снова синхронизируется без потери
  полученных сообщений.
- `connection_state_survives_absence_of_observers` — регрессия: состояние
  соединения не теряется, если клиент успел подключиться до появления
  первого подписчика; `register/login` не зависают с `NotConnected`.

## 6. Ручной smoke (критерии приёмки)

Скрипт ниже не требует TLS (режим `--plaintext-insecure` допустим только на
loopback) и использует `--password-stdin`/`RCHAT_PASSWORD`, так что
выполняется неинтерактивно. Для TLS-режима см. README.

```bash
cargo build --bin rchat-server --bin rchat-cli
WORK=$(mktemp -d)
DB=$WORK/server.sqlite3
PORT=18443

# 1. Сервер (в фоне) в dev-режиме.
./target/debug/rchat-server --db "$DB" --listen 127.0.0.1:$PORT \
    --plaintext-insecure --allow-dev-plaintext-chats &
SERVER_PID=$!
sleep 1

export RCHAT_SERVER=ws://127.0.0.1:$PORT
# 2. Регистрация и чат.
export RCHAT_PASSWORD=secret-alice
./target/debug/rchat-cli --data-dir "$WORK/alice" register alice
RCHAT_PASSWORD=secret-bob ./target/debug/rchat-cli --data-dir "$WORK/bob" register bob
CHAT=$(./target/debug/rchat-cli --data-dir "$WORK/alice" chat-create bob \
    | grep -oE '[0-9a-f]{32}' | head -n1)
./target/debug/rchat-cli --data-dir "$WORK/alice" send "$CHAT" Привет
./target/debug/rchat-cli --data-dir "$WORK/bob" sync
./target/debug/rchat-cli --data-dir "$WORK/bob" history "$CHAT"

# 3. Офлайн-очередь: останавливаем сервер, отправляем, поднимаем снова и
#    запускаем `sync` — он дожидается соединения и выгружает outbox.
kill $SERVER_PID
./target/debug/rchat-cli --data-dir "$WORK/alice" send "$CHAT" "из офлайна" --wait 1
./target/debug/rchat-server --db "$DB" --listen 127.0.0.1:$PORT \
    --plaintext-insecure --allow-dev-plaintext-chats &
SERVER_PID=$!
sleep 1
./target/debug/rchat-cli --data-dir "$WORK/alice" sync
./target/debug/rchat-cli --data-dir "$WORK/alice" status

# 4. Правка/удаление и квитанции (server_id — номер после '#' в history).
MSG_ID=$(./target/debug/rchat-cli --data-dir "$WORK/alice" history "$CHAT" \
    | grep -oE '#[0-9]+' | tr -d '#' | head -n1)
./target/debug/rchat-cli --data-dir "$WORK/alice" edit "$CHAT" "$MSG_ID" "Новый текст"
./target/debug/rchat-cli --data-dir "$WORK/bob" sync
./target/debug/rchat-cli --data-dir "$WORK/bob" read "$CHAT"
./target/debug/rchat-cli --data-dir "$WORK/alice" sync   # догрузить квитанцию
./target/debug/rchat-cli --data-dir "$WORK/alice" receipts "$CHAT"
./target/debug/rchat-cli --data-dir "$WORK/alice" delete "$CHAT" "$MSG_ID"

# 5. Групповой E2E-чат: создание, отправка, приглашение, выход и исключение
#    (ключ ротируется — исключённый не читает новые сообщения).
RCHAT_PASSWORD=secret-carol ./target/debug/rchat-cli --data-dir "$WORK/carol" register carol
GROUP=$(./target/debug/rchat-cli --data-dir "$WORK/alice" group-create Проект \
    --members bob,carol | grep -oE '[0-9a-f]{32}' | head -n1)
./target/debug/rchat-cli --data-dir "$WORK/alice" send "$GROUP" "Всем привет"
./target/debug/rchat-cli --data-dir "$WORK/bob" sync
./target/debug/rchat-cli --data-dir "$WORK/bob" group info "$GROUP"
./target/debug/rchat-cli --data-dir "$WORK/carol" group leave "$GROUP"
./target/debug/rchat-cli --data-dir "$WORK/alice" sync   # состав без carol
./target/debug/rchat-cli --data-dir "$WORK/alice" send "$GROUP" "После выхода"
./target/debug/rchat-cli --data-dir "$WORK/bob" sync
./target/debug/rchat-cli --data-dir "$WORK/bob" history "$GROUP"   # оба текста
./target/debug/rchat-cli --data-dir "$WORK/carol" sync
./target/debug/rchat-cli --data-dir "$WORK/carol" history "$GROUP" # только первый
./target/debug/rchat-cli --data-dir "$WORK/alice" group remove "$GROUP" bob

# 6. Профили аккаунтов в общем каталоге данных: именованный профиль,
#    список (текущий помечен `*`) и удаление только его локальных данных.
DATA=$WORK/profiles
RCHAT_PASSWORD=secret-dave ./target/debug/rchat-cli --data-dir "$DATA" \
    --account work register dave
./target/debug/rchat-cli --data-dir "$DATA" --account work accounts
./target/debug/rchat-cli --data-dir "$DATA" account-delete work --yes

kill $SERVER_PID
```

Проверяемые свойства: регистрация двух пользователей; личный чат; доставка и
история; офлайн-сообщение уходит при следующем `sync` (он дожидается
соединения и выгружает outbox); `status` показывает пустую очередь;
правка/удаление и receipts видны обоим; после рестарта сервера история
сохраняется; групповая отправка доходит участникам, а покинувшая группу не
читает сообщения после выхода (ротация megolm-сессии) и не может писать;
`accounts` показывает профили, `account-delete` убирает только выбранный
профиль. То же самое для TLS-режима — команды из README.

### Звонки вручную

Звонок живёт, пока caller удерживает соединение (`call invite --wait`), а
callee находится в сети. Скрипт выше их не автоматизирует — нужны
одновременно несколько окон (и для удобства второе устройство callee).
В примерах ниже подставьте `WORK`, `CHAT` и `CALL_ID` из скрипта/watch:

```bash
# Терминал 1: Bob слушает события и остаётся online.
./target/debug/rchat-cli --data-dir "$WORK/bob" watch

# Терминал 2: Alice приглашает и ждёт; печатается call_id.
./target/debug/rchat-cli --data-dir "$WORK/alice" call invite "$CHAT" --wait

# В watch у Bob появляется `[звонок] входящий: <call_id>`.
# CALL_ID=<call_id>. Второе устройство Bob принимает звонок:
RCHAT_PASSWORD=secret-bob ./target/debug/rchat-cli \
    --data-dir "$WORK/bob2" login bob
./target/debug/rchat-cli --data-dir "$WORK/bob2" call accept "$CALL_ID"

# Alice и watch у Bob печатают `[звонок] принят`; медиа нет.
# Завершает любой участник; caller выходит из --wait:
./target/debug/rchat-cli --data-dir "$WORK/bob2" call end "$CALL_ID"

# Отклонение: повторить invite --wait и выполнить call decline "$CALL_ID".
# В групповом чате приглашение отклоняется: call invite "$GROUP" → ошибка.
```

Проверяются: transient-события у обоих участников и всех устройств,
роли (accept/decline — только callee, end — любой участник), запрет
групповых звонков, завершение звонка при выходе caller из `--wait`.
Автоматически это покрыто серверными и E2E-тестами звонков (см. §3).

## 7. Особенности

- Backoff реконнекта и джиттер не фиксированы по времени: ассерты по
  состоянию, не по задержке.
- Testkit ускоряет Argon2, поэтому CLI smoke не тратит время на KDF;
  параметры продакшена проверяются отдельно конфигурацией по умолчанию.
- `rchat-e2e-tests` держит общий харнесс в `tests/common/mod.rs`
  (`start_server()`, `build_client(...)`, ожидания и SQL-проверки «на
  сервере нет plaintext»); каждый файл тестов компилирует его отдельно.

## 8. Известные непокрытые случаи

- **Property-based/fuzz-тесты** публичного декодера не подключены (§12.4
  спецификации не считает их блокером для первого релиза). Вместо этого
  «мок-стек» точечно внедряет обрезанный protobuf-кадр, а юнит-тесты схемы
  проверяют неизвестные поля, enum-значения и ветви `oneof`.
- **Обновление клиентской схемы «с предыдущей версии»** проверено точечно:
  юнит-тесты `rchat-client-storage-sqlite` поднимают БД с v1 (данные
  сохраняются, прежним участникам достаётся роль 0) и проверяют
  идемпотентность повторной миграции; общая проверка «любая старая версия →
  текущая» на все будущие миграции не автоматизирована. Серверная БД
  дополнительно проверяется тестом `migration_upgrade_preserves_data`
  (v1 → текущая).
- **Нагрузочное тестирование** отсутствует: распределённый флуд и исчерпание
  соединений не воспроизводятся, лимиты частоты проверяются юнит-тестами.
- **Права файлов `0600/0700`** и поведение `SecretStore` на не-Unix
  платформах не покрыты (CI — Linux; ограничение зафиксировано в
  `docs/threat-model.md`).
- **Тестовые E2E-ключи-фикстуры** намеренно не используются: vodozemac не
  допускает детерминированных ключей, поэтому каждый E2E-тест создаёт
  реальные случайные идентичности; фикстуры `fixtures` обслуживают только
  не-криптографические структуры.

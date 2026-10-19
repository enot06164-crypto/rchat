# Python SDK для rchat

Официальные Python-биндинги к `rchat-ffi`. Работают через стандартный
`ctypes`, без сторонних зависимостей. Есть два уровня API:

- `rchat.ffi` — низкоуровневая обёртка над C-ABI: JSON-запросы, JSON-ответы,
  события и явное управление строками;
- `rchat.RChatClient` — высокоуровневый клиент с фоновым читателем событий,
  контекстным менеджером и dataclass/dict-моделями.

Полная схема запросов, ответов, событий и ошибок — в
[`docs/ffi.md`](../../docs/ffi.md). C-заголовок — в
[`crates/rchat-ffi/include/rchat.h`](../../crates/rchat-ffi/include/rchat.h).

## Требования

- Python 3.9 или новее;
- собранная `librchat_ffi.so` / `librchat_ffi.dylib` / `rchat_ffi.dll`;
- для запуска клиента — работающий `rchat-server`.

## Сборка библиотеки

Из корня репозитория:

```bash
cargo build -p rchat-ffi
```

Биндинг ищет библиотеку в таком порядке:

1. файл или каталог из `RCHAT_LIB_PATH`;
2. `target/release/` относительно корня репозитория;
3. `target/debug/` относительно корня репозитория.

Для release-сборки:

```bash
cargo build -p rchat-ffi --release
export RCHAT_LIB_PATH="$PWD/target/release"
```

## Установка

Пакет можно установить в текущее окружение:

```bash
python3 -m pip install -e bindings/python
```

Или использовать без установки из корня репозитория:

```bash
python3 bindings/python/examples/smoke_ping.py
```

Скрипты сами добавляют `bindings/python` в `sys.path`, поэтому запускаются
из корня репозитория.

## Быстрый старт

```python
from rchat import RChatClient

with RChatClient(
    data_dir="/tmp/rchat/alice",
    server="wss://chat.example",
    tls_fingerprint="AA:BB:...",  # либо tls_insecure=True только для dev
) as client:
    client.login("alice", "password")

    chat = client.create_direct_chat("bob", mode="private")
    client.send_message(chat.chat_id, "привет")

    for event in client.events():
        if event.event == "message_received":
            print(event.data["chat_id"], event.data.get("body"))
```

`RChatClient` запускает фоновый daemon-поток, который читает
`rchat_receive_json` и складывает события в `queue.Queue`. Поток
останавливается до `rchat_destroy`, поэтому закрытие через `close()` или
`with` не оставляет обращений к освобождённому handle.

Если профиль уже сохранил токен, `login` не нужен: фоновая сессия
восстанавливается при старте, а её состояние доступно через `get_me()` и
`get_session()`. Повторный вызов `login` для такого профиля возвращает
`invalid_request: already authenticated` — так же, как одноимённая команда
FFI.

### События

```python
event = client.poll_event(timeout=1.0)  # Event | None; timeout в секундах
if event is not None:
    print(event.event, event.data)

for event in client.events():
    ...
    if event.event == "event_lagged":
        # События потеряны: перечитайте локальное состояние.
        chats = client.get_chats()
        messages = client.get_messages(chat_id)
```

`event.data` — обычный `dict`. Модель `Event` одновременно является
`dict`-подобной: доступны `event.event`, `event.data`, `event["event"]`.

### Модели

`Chat`, `Message`, `Call`, `Session`, `Event` и `AvatarInfo` — dataclass-модели
и одновременно подклассы `dict`. Это позволяет писать и так:

```python
chat = client.create_direct_chat("bob")
print(chat.chat_id, chat["chat_id"], chat.title)

print(chat.avatar.initials, chat.avatar.color_hex)
print(chat["avatar"]["initials"], chat["avatar"]["color_index"])
```

`avatar` содержит `initials`, `color_index` (`0..=7`) и `color_hex`; цвет
детерминирован по `user_id`/`chat_id`, инициалы безопасно считаются по
Unicode-имени. У `Message` поле `avatar` необязательное (FFI пока не
прикладывает его к сообщениям; зарезервировано для будущих версий).

Неизвестные поля из будущих версий FFI игнорируются; `to_dict()` возвращает
обычный `dict`. Формальная схема всех запросов/ответов/событий —
`schema/rchat.schema.json` (см. `docs/ffi.md` §11).

### Ошибки

Высокоуровневые методы бросают подклассы `RChatError`
(`InvalidRequestError`, `AuthenticationError`, `TransportError`,
`NotFoundError`, `E2eUnavailableError` и т.д.):

```python
from rchat import RChatError

try:
    client.send_message(chat_id, text)
except RChatError as error:
    if error.retryable:
        ...  # повторить позже
    print(error.code, error.message, error.server_code)
```

`error.server_code`/`error.server_code_name` заполнены для ошибок
`code == "server"`. Ошибка локальной конфигурации — `RChatConfigError`,
закрытый клиент — `RChatClosedError`.

## Низкоуровневый API

```python
from rchat import ffi

print(ffi.rchat_version(), ffi.rchat_api_version())

handle = ffi.rchat_create({
    "data_dir": "/tmp/rchat-ping",
    "server": "ws://127.0.0.1:8443",
    "auto_start": False,
})
if handle is None:
    raise RuntimeError(ffi.rchat_last_error())

try:
    response = ffi.rchat_send_json(handle, {"@type": "ping"})
    print(response)
finally:
    ffi.rchat_destroy(handle)
```

Правила управления памятью:

- строки из `rchat_send_json` и `rchat_receive_json` обёртка копирует и
  освобождает через `rchat_free_string` в `finally`;
- строки из `rchat_version` и `rchat_last_error` заимствованные; обёртка их
  не освобождает;
- низкоуровневые функции возвращают обычные `dict`/`None`, а ошибки
  конфигурации `rchat_create` — `None` плюс текст в `rchat_last_error()`.

## Примеры

- `examples/smoke_ping.py` — создание handle, `ping`, версия, корректное
  освобождение; сервер не нужен (`auto_start=False`);
- `examples/echo_bot.py` — вход, прослушивание событий и ответ эхом.

## WebSocket-шлюз для веб-клиента (этап H)

Модуль `rchat.gateway` открывает `ws://…/ws` и проксирует протокол FFI в
браузер: запросы — в `rchat_send_json`, события — всем открытым вкладкам.
Один процесс шлюза владеет ровно одним профилем (создаёт `RChatClient`).

```bash
python3 -m venv .venv-gateway
.venv-gateway/bin/pip install websockets      # единственная внешняя зависимость шлюза
PYTHONPATH=bindings/python .venv-gateway/bin/python -m rchat.gateway \
    --data-dir /tmp/rchat-web/alice \
    --server ws://127.0.0.1:9000 \
    --port 8080
```

Флаги: `--data-dir` (обязателен), `--server` (обязателен), `--host/--port`
(по умолчанию `127.0.0.1:8080`), `--/--no-e2e` (E2E по умолчанию включён),
`--tls-fingerprint`/`--tls-insecure` для `wss://`. Прочие пути (не `/ws`)
получают JSON 404 — статику отдаёт Vite (`npm run dev`/`npm run preview` в
`apps/web`). Протокольный смок двух профилей —
`apps/web/scripts/smoke_gateway.py`.

## Тесты

Из корня репозитория:

```bash
cargo build -p rchat-ffi
cargo build --bin rchat-server
python3 -m unittest discover -s bindings/python/tests
```

`test_ffi_lowlevel.py` проверяет версии, ctypes-сигнатуры, создание handle,
`ping`, ошибки конфигурации и освобождение строк (включая проверку, что
заимствованные строки не освобождаются). `test_client_e2e.py` поднимает
настоящий `rchat-server` во временном каталоге, регистрирует Alice и Bob,
создаёт E2E-чат, отправляет сообщение и ждёт его в событиях Bob.

## Потоки и ограничения

- Один `RChatClient` — один профиль/каталог данных. Не открывайте один
  каталог одновременно из CLI и SDK: файл секретов однопроцессный.
- `rchat_send_json` синхронный; вызовы из разных потоков сериализуются
  клиентом. Не вызывайте `close()` из потока событий.
- События — сигналы, источник истины — локальное хранилище. При
  `event_lagged` перечитайте чаты/сообщения/receipts.
- Звонки — только сигнализация `invite/accept/decline/end`; медиа и WebRTC
  не реализованы.
- `ws://` и `tls_insecure` допустимы только для локальной разработки.
- SDK работает в процессе хоста и видит расшифрованные сообщения и пароль
  при `login`/`register`. Это доверенная граница приложения.

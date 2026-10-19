# C-ABI и JSON-интерфейс (`rchat-ffi`)

Документ описывает встраиваемый интерфейс клиента: C-ABI с JSON-запросами,
JSON-ответами и очередью событий. Интерфейс предназначен для хостов на C,
C++, Python, C#, Go и других языках. В поставку входят библиотека, заголовок
`crates/rchat-ffi/include/rchat.h` и официальный Python SDK на `ctypes`
(`bindings/python`, см. §10).

Один `RchatHandle` — один независимый клиент/профиль: собственный каталог
данных, SQLite, файл секретов и E2E-состояние. Несколько handle в одном
процессе не делят состояние и могут работать с разными аккаунтами и даже
разными серверами.

## 1. Состав и сборка

Крейт `rchat-ffi` собирается как `cdylib`, `staticlib` и `rlib`:

```bash
cargo build -p rchat-ffi --release
```

Артефакты зависят от платформы:

- Linux: `target/release/librchat_ffi.so`, `target/release/librchat_ffi.a`;
- macOS: `target/release/librchat_ffi.dylib`, `target/release/librchat_ffi.a`;
- Windows: `target/release/rchat_ffi.dll`, `target/release/rchat_ffi.lib`
  (точные имена зависят от toolchain).

Динамическая библиотека предпочтительна: статическая содержит весь Rust
runtime и при линковке может потребовать системные библиотеки платформы.
Заголовок подключается как:

```c
#include "rchat.h"
```

## 2. Жизненный цикл

```c
#include <stdio.h>
#include "rchat.h"

RchatHandle *handle = rchat_create(
    "{\"data_dir\":\"/tmp/rchat-ffi\","
    "\"server\":\"wss://chat.example\","
    "\"auto_start\":true}");

if (handle == NULL) {
    const char *error = rchat_last_error(); /* заимствованная строка */
    fprintf(stderr, "rchat_create: %s\n", error ? error : "unknown error");
    return 1;
}

char *response = rchat_send_json(handle, "{\"@type\":\"ping\"}");
if (response != NULL) {
    puts(response);
    rchat_free_string(response);
}

/* timeout_ms = 1000; NULL означает тайм-аут/пустую очередь. */
char *event = rchat_receive_json(handle, 1000);
if (event != NULL) {
    puts(event);
    rchat_free_string(event);
}

rchat_destroy(handle);
```

Правила:

- `rchat_create` возвращает `NULL` при ошибке конфигурации или инициализации.
  Текст ошибки для текущего потока — `rchat_last_error()`.
- `rchat_destroy(NULL)` допустим. После `rchat_destroy` handle нельзя
  использовать; повторное освобождение — неопределённое поведение.
- `rchat_send_json` синхронный: он блокирует вызывающий поток до ответа
  (включая сетевые операции и тайм-ауты ядра). Не вызывайте его из потока,
  который обязан оставаться отзывчивым.
- `rchat_receive_json` забирает одно событие. `timeout_ms = 0` — без
  ожидания; значения больше 24 часов ограничиваются. `NULL` — тайм-аут или
  пустая очередь.
- Строки, возвращённые `rchat_send_json` и `rchat_receive_json`, освобождает
  вызывающая сторона через `rchat_free_string`.
- `rchat_version()` и `rchat_last_error()` возвращают заимствованные строки;
  освобождать их нельзя.
- Handle можно использовать из нескольких потоков. Вызовы `send`/`receive`
  сериализуются внутри; события из очереди достаются по одному.
  `rchat_destroy` нельзя вызывать одновременно с другими операциями над тем
  же handle.
- Из Rust-хоста не вызывайте функции FFI внутри потока tokio-runtime:
  библиотека использует собственный runtime и `block_on`.

## 3. Конфигурация `rchat_create`

Конфигурация — JSON-объект. Неизвестные поля отклоняются, чтобы опечатки не
игнорировались молча.

| Поле | Тип | По умолчанию | Описание |
| --- | --- | --- | --- |
| `data_dir` | string | обязательно | Каталог профиля. Создаётся; внутри — SQLite и секреты. Один handle — один профиль. |
| `server` | string | обязательно | `ws://host:port[/path]` (только dev) или `wss://host:port[/path]`. |
| `tls_fingerprint` | string \| null | `null` | SHA-256 отпечаток сертификата в hex; разделители `:`, `-`, `_`, пробелы допустимы. Взаимоисключающе с `tls_insecure`. |
| `tls_insecure` | bool | `false` | Отключить проверку TLS. Только для явного локального тестирования. |
| `e2e` | bool | `true` | Подключить E2E-провайдер (Olm/Megolm). Для dev-чатов без E2E установите `false` и передавайте `e2e:false` при создании чата. |
| `auto_start` | bool | `true` | Запустить фоновый актор сразу. При `false` сетевые команды запускают его сами, а `start` делает это явно. |
| `device_name` | string \| null | `"rchat-ffi"` | Имя устройства в списке устройств аккаунта. |
| `connect_timeout_ms` | integer | `10000` | Тайм-аут соединения/handshake, `1..=600000`. |
| `request_timeout_ms` | integer | `20000` | Тайм-аут одного запроса к серверу, `1..=600000`. |

`data_dir` совместим с профилем CLI (`client.sqlite3`, `secrets.bin`,
`secrets.key`), поэтому один и тот же профиль можно открыть из CLI или FFI
(но не одновременно: хранилище секретов однопроцессное). Логика `--account`
и `accounts` — особенность CLI; FFI-хост сам выбирает каталог профиля.

Вне локальной разработки используйте `wss://` с системными корнями или
пиннингом `tls_fingerprint`. `ws://` и `tls_insecure` не обеспечивают защиту
канала. Пароль не хранится в конфигурации: он передаётся только в запросах
`register`/`login` и не попадает в ответы, события или логи FFI.

## 4. Формат запросов и ответов

Запрос — JSON-объект с обязательным строковым полем `@type`:

В примерах `<chat_id>`, `<user_id>`, `<device_id>`, `<local_id>` и
`<call_id>` — строковые идентификаторы; сервер выдаёт 32 hex-символа.

```json
{"@type":"send_message","chat_id":"<chat_id>","text":"привет"}
```

Успешный ответ:

```json
{
  "@type": "ok",
  "request": "send_message",
  "data": {"local_id": "<local_id>"}
}
```

Ошибка:

```json
{
  "@type": "error",
  "request": "send_message",
  "code": "server",
  "message": "user is busy",
  "retryable": false,
  "server_code": 12,
  "server_code_name": "conflict"
}
```

Поля ошибки:

- `code` — стабильный код FFI (см. §6);
- `message` — диагностическое сообщение; текст сервера следует считать
  недоверенным и не показывать без экранирования;
- `retryable` — имеет смысл ли повторить операцию (для серверных ошибок —
  значение сервера, для транспорта/тайм-аута — `true`);
- `server_code`/`server_code_name` — только для `code == "server"`.

Неизвестные поля запроса игнорируются; это оставляет место для расширения.
`@type` должен быть строкой. Некорректный JSON, не-объект, отсутствие `@type`,
невалидный UTF-8 и NULL-указатели дают `invalid_request`/`invalid_handle` (или
`NULL` для `receive`).

## 5. Команды

Все идентификаторы (`chat_id`, `local_id`, `server_id`, `user_id`,
`device_id`, `call_id`) — строки/числа в том виде, в каком их вернуло ядро.
`*_server_id` — целые числа; `null` допустим там, где указано.

### 5.1. Сессия и состояние

| `@type` | Поля | `data` |
| --- | --- | --- |
| `ping` | — | `{service, version, api_version}` |
| `start` | — | `{state}` |
| `get_connection_state` | — | `{state}` |
| `wait_online` | `timeout_ms?` (по умолчанию 5000, до 600000) | `{state}` |
| `get_session` | — | объект сессии или `null` |
| `get_account` | — | `{user_id, username, display_name, device_id}` или `null` |
| `get_status` | — | объект статуса |
| `register` | `username`, `display_name?`, `password` | объект сессии |
| `login` | `username`, `password` | объект сессии |
| `logout` | `wipe?` (bool, по умолчанию false) | `{wiped}` |

Состояния соединения: `disconnected`, `connecting`, `unauthenticated`,
`connected`, `reconnecting`.

Объект сессии:

```json
{
  "active": true,
  "user_id": "<user_id>",
  "username": "alice",
  "display_name": "Alice",
  "device_id": "<device_id>",
  "avatar": {"initials": "AL", "color_index": 5, "color_hex": "#65AADD"}
}
```

`active:false` означает, что локальный аккаунт есть, но соединение/сессия ещё
не активны. Если аккаунта нет — `data:null`. `logout` без `wipe` удаляет
токен сессии, с `wipe:true` — также локальные данные и криптосостояние.

Объект статуса:

```json
{
  "connection": "connected",
  "session": {"active": true, "...": "..."},
  "account": {
    "user_id": "...",
    "username": "...",
    "display_name": "...",
    "device_id": "...",
    "avatar": {"initials": "AL", "color_index": 5, "color_hex": "#65AADD"}
  },
  "sync_cursor": 42,
  "outbox": {"total": 1, "due": 1},
  "data_dir": "/tmp/rchat-ffi",
  "server": "wss://chat.example",
  "device_name": "rchat-ffi",
  "e2e": true
}
```

`outbox.due` — сообщения, готовые к отправке на текущий момент; `total` —
все незавершённые записи очереди.

### 5.2. Чаты

| `@type` | Поля | `data` |
| --- | --- | --- |
| `get_chats` | — | массив чатов из локального кеша |
| `refresh_chats` | — | массив чатов с сервера (сеть) |
| `get_chat_members` | `chat_id` | массив участников |
| `create_direct_chat` | `peer_username`, `e2e?` (по умолчанию true) | чат |
| `create_group_chat` | `name`, `members?` (usernames), `e2e?` (true) | чат |
| `invite_to_chat` | `chat_id`, `usernames` | чат |
| `leave_chat` | `chat_id` | `null` |
| `remove_chat_member` | `chat_id`, `user_id` | чат |
| `rename_chat` | `chat_id`, `name` | чат |

`get_chats` не требует сети и возвращает локальный кеш; для актуализации
используйте `refresh_chats` или фоновую синхронизацию. `get_chat_members`
также читает локальный кеш; для незнакомого `chat_id` возвращает пустой
массив.

Объект чата:

```json
{
  "chat_id": "<chat_id>",
  "kind": "direct",
  "e2e_mode": "olm_v1",
  "title": "Bob",
  "peer_user_id": "<user_id>",
  "created_at_ms": 1700000000000,
  "updated_at_ms": 1700000000000,
  "last_server_id": 7,
  "avatar": {"initials": "BO", "color_index": 1, "color_hex": "#FAA774"}
}
```

`kind`: `direct`, `group`, `unspecified`; `e2e_mode`: `olm_v1`, `none_dev`,
`unspecified`. Для группового чата `peer_user_id` — `null`.

Объект участника:

```json
{
  "user_id": "<user_id>",
  "username": "bob",
  "display_name": "Bob",
  "role": "owner",
  "joined_at_ms": 1700000000000,
  "avatar": {"initials": "BO", "color_index": 1, "color_hex": "#FAA774"}
}
```

`role`: `owner`, `member`, `unspecified`.

### 5.3. Сообщения, история и receipts

| `@type` | Поля | `data` |
| --- | --- | --- |
| `send_message` | `chat_id`, `text?`, `attachment?`, `reply_to_server_id?` | `{local_id}` |
| `download_attachment` | `server_id` | `{kind, name, mime, size, data_base64}` |
| `get_messages` | `chat_id`, `before_server_id?`, `limit?` | массив сообщений (локально) |
| `fetch_history` | `chat_id`, `before_server_id?`, `limit?` | массив сообщений (с сервера) |
| `edit_message` | `chat_id`, `server_id`, `text` | сообщение |
| `delete_message` | `chat_id`, `server_id` | сообщение |
| `mark_read` | `chat_id`, `up_to_server_id?` | `{up_to_server_id}` |
| `get_receipts` | `chat_id` | массив receipts |
| `sync` | — | объект прогресса синхронизации |

`limit` — `1..=100`, по умолчанию 50 (серверный предел страницы). История и сообщения возвращаются в
порядке хранилища: новые первыми. `send_message` возвращает локальный id
сразу после постановки в очередь; результат отправки приходит событием
`outbox_changed` (или виден в `status` сообщения после `get_messages`).
Пустой/пробельный текст отклоняется.

`mark_read` без `up_to_server_id` использует `last_server_id` чата; если
отмечать нечего, возвращает `0`.

### 5.4. Вложения (файл, фото, видео, голос)

Вложение едет **отдельным блобом**, как в Telegram: клиент шифрует файл
случайным симметричным ключом (XChaCha20-Poly1305) и заливает **шифротекст**
на сервер частями; в зашифрованное сообщение кладётся только ссылка на блоб и
ключ. Сервер видит шифротекст файла, но не ключ и не содержимое.

**Где лежат файлы.** Содержимое блоба хранится **на диске сервера** (каталог
`blobs` рядом с БД, файл `<blob_id>.blob`); в базе остаются только
метаданные — путь, размер, владелец, чат и непрозрачный токен. Сама база
поэтому не раздувается от файлов.

Кадры канала файлов (60..=63):

| `@type` | Поля | `data` |
| --- | --- | --- |
| `upload_blob` | `blob_id`, `chat_id`, `offset`, `total_size`, `data` | `{received}` (ack для докачки) |
| `finish_blob` | `blob_id`, `chat_id`, `total_size` | `{blob_ref, size}` |
| `download_blob` | `blob_ref`, `offset`, `length` | `{data, total_size, eof}` |
| `abort_blob` | `blob_id` | `null` |

- `blob_id` выбирает клиент (им же докачивается: повтор той же части идемпотентен);
- `blob_ref` — непрозрачная ссылка, выдаётся сервером при `finish_blob`; без неё
  файл не скачать;
- скачать блоб может только участник чата, к которому файл привязан; отменить
  незавершённую загрузку может только её владелец;
- лимит вложения — 16 МиБ (`MAX_ATTACHMENT_BYTES`).

Сообщение с вложением (конверт `{t, a}` шифруется целиком, olm/megolm):

`send_message` с вложением:

```json
{
  "@type": "send_message",
  "chat_id": "<chat_id>",
  "text": "смотри",
  "attachment": {
    "kind": "photo",
    "name": "pic.png",
    "mime": "image/png",
    "size": 20480,
    "data_base64": "<base64>"
  }
}
```

- `kind`: `photo`, `video`, `file`, `voice`;
- `text` можно опустить, если есть вложение (но не пустые оба);
- `data_base64` — содержимое файла (ядро зашифрует его и зальёт блобом
  само; ключ и ссылка попадут в сообщение автоматически);
- `thumb_base64` / `thumb_mime` — постер-кадр видео (идёт **отдельным
  блобом**, как thumbnail в Telegram);
- `waveform_base64` — пики звуковой волны голосового (48 байт) для
  отрисовки в UI;
- несколько вложений разом — альбом: `{"attachments": [...]}`, ядро отправит
  их сообщениями с общим `grouped_id` (в объекте сообщения он виден как
  `grouped_id`);
- при пересборке outbox (новые prekey) конверт шифруется заново целиком;
  содержимое блоба не перезаливается.

Особенности клиента:
- **дедупликация**: файл, который уже заливался с этого устройства, не
  заливается повторно — по SHA-256 содержимого ищется локальный кэш
  загрузок (ключ остаётся на устройстве, сервер его не знает). Это ровно
  модель Telegram: там ключ файла тоже принадлежит отправителю, поэтому
  глобальная дедупликация по содержимому между разными устройствами
  невозможна без раскрытия ключа серверу;
- **кэш**: скачанный файл сохраняется локально (лимит 512 МиБ, старые
  вытесняются), повторное открытие не обращается к сети.

`get_messages`/`fetch_history` возвращают в объекте сообщения **метаданные**
вложения (без байтов), чтобы не гонять содержимое списком:

```json
"attachment": {"kind": "photo", "name": "pic.png", "mime": "image/png", "size": 20480}
```

Содержимое забирается отдельно — ядро само скачивает блоб (частями) и
расшифровывает его ключом из сообщения:

```json
{"@type": "download_attachment", "server_id": 42}
```

Ответ: `{"kind": "photo", "name": "pic.png", "mime": "image/png", "size": 20480,
"data_base64": "..."}`.

Объект сообщения:

```json
{
  "local_id": "<local_id>",
  "server_id": 42,
  "chat_id": "<chat_id>",
  "sender_user_id": "<user_id>",
  "sender_device_id": "<device_id>",
  "content_kind": "text",
  "body": "привет",
  "attachment": null,
  "reply_to_server_id": null,
  "status": "none",
  "created_at_ms": 1700000000000,
  "edited_at_ms": null,
  "deleted_at_ms": null
}
```

`server_id`, `body`, `attachment`, `reply_to_server_id` — `null`, если значение
неизвестно (например, сообщение ещё в очереди или было удалено). `content_kind`
для вложения равен его виду (`photo`, `video`, `file`, `voice`), для текста —
`text`. `status`:
`pending`, `sending`, `sent`, `failed`, `none` (для входящих сообщений).

Объект receipt:

```json
{
  "chat_id": "<chat_id>",
  "user_id": "<user_id>",
  "delivered_up_to": 40,
  "read_up_to": 39,
  "updated_at_ms": 1700000000000
}
```

Объект прогресса синхронизации:

```json
{
  "from_seq": 41,
  "to_seq": 50,
  "events_applied": 9,
  "events_skipped": 0,
  "resync_performed": false
}
```

### 5.4. Устройства

| `@type` | Поля | `data` |
| --- | --- | --- |
| `get_devices` | — | массив устройств текущего аккаунта (сеть) |
| `refresh_devices` | `user_ids` | `null` |
| `revoke_device` | `device_id` | `null` |

`refresh_devices` обновляет кеш устройств указанных пользователей, чтобы
отправка в E2E-чаты использовала актуальные prekey. `revoke_device` отзывает
устройство текущего аккаунта.

Объект устройства:

```json
{
  "device_id": "<device_id>",
  "user_id": "<user_id>",
  "name": "rchat-ffi",
  "platform": "linux",
  "e2e_version": 1,
  "ed25519": "hex...",
  "curve25519": "hex...",
  "revoked_at_ms": null
}
```

Открытые identity-ключи кодируются hex. Секретов в объекте нет.

### 5.5. Звонки

| `@type` | Поля | `data` |
| --- | --- | --- |
| `call_invite` | `chat_id` (только личный чат) | объект звонка |
| `call_accept` | `call_id` | объект звонка |
| `call_decline` | `call_id` | объект звонка |
| `call_end` | `call_id` | объект звонка |

Это только сигнализация: медиа и WebRTC не реализованы. Состояние звонка
живёт в памяти сервера и клиента. `call_invite` создаёт звонок; callee
получает событие `call_invited`, accept/decline доступны только callee,
завершить звонок может любой участник. Приглашение ограничено rate-limit
сервера; callee должен быть онлайн. Если callee полностью офлайн, сервер
завершает звонок.

Объект звонка:

```json
{
  "call_id": "<call_id>",
  "chat_id": "<chat_id>",
  "caller_user_id": "<user_id>",
  "callee_user_id": "<user_id>",
  "state": "ringing",
  "created_at_ms": 1700000000000,
  "accepted_at_ms": null
}
```

`state`: `ringing`, `active`, `unknown`. Тайм-ауты (ringing 60 с, active 4 ч)
применяются лениво; хост должен сам снимать кольцевой тайм-аут и не ждать
отдельного события по времени.

### 5.6. Аватары

Сессии, аккаунты, чаты и участники содержат вложенный объект `avatar` с
инициалами и детерминированным цветом — UI-клиентам не нужно дублировать
эту логику:

```json
{"initials": "АИ", "color_index": 5, "color_hex": "#65AADD"}
```

Инициалы (`initials`) считаются по `display_name`, с фоллбэком на `username`
(у чата — на `title`):

- слова разделяются Unicode-пробелами, из каждого берутся только буквы
  (`char::is_alphabetic`), поэтому эмодзи, цифры и пунктуация безопасно
  пропускаются;
- два и более слов — первые буквы первых двух (`"Иван Петров"` → `"ИП"`);
- одно слово — до двух первых букв (`"Алиса"` → `"АЛ"`);
- если букв нет ни в имени, ни в фоллбэке — пустая строка `""`.

`color_index` — стабильный индекс `0..=7`: FNV-1a 64 от UTF-8 `user_id` /
`chat_id` по модулю 8. Один и тот же идентификатор даёт один цвет на всех
устройствах. `color_hex` — соответствующий цвет палитры:

| `color_index` | `color_hex` | Цвет |
| --- | --- | --- |
| 0 | `#E17076` | красный |
| 1 | `#FAA774` | оранжевый |
| 2 | `#A695E7` | фиолетовый |
| 3 | `#7BC862` | зелёный |
| 4 | `#6EC9CB` | бирюзовый |
| 5 | `#65AADD` | синий |
| 6 | `#EE7AAE` | розовый |
| 7 | `#E5CA77` | жёлтый |

Реализация — `crates/rchat-core/src/avatar.rs`; JSON-объект собирается в
`crates/rchat-ffi/src/json.rs`. Python SDK отдаёт его как `AvatarInfo`
(см. §10), включая необязательное поле у `Message` для будущих версий.

## 6. Коды ошибок FFI

| `code` | Значение |
| --- | --- |
| `invalid_request` | Некорректный JSON/поля/значение. |
| `invalid_handle` | Передан NULL-handle. |
| `unknown_method` | Неизвестный `@type`. |
| `server` | Сервер вернул ошибку; см. `server_code`/`server_code_name`. |
| `transport` | Ошибка сети/соединения. |
| `storage` | Ошибка локального хранилища. |
| `secret_store` | Ошибка файла секретов. |
| `crypto` | Ошибка E2E-провайдера. |
| `not_authenticated` | Нет активной аутентификации. |
| `protocol` | Нарушение протокола/неожиданный ответ. |
| `not_found` | Объект не найден. |
| `e2e_unavailable` | E2E недоступен/не сконфигурирован. |
| `not_connected` | Нет соединения; операция может быть повторена. |
| `timeout` | Тайм-аут операции. |
| `closed` | Клиент остановлен. |
| `panic` | Внутренняя паника Rust, перехваченная на границе (не должна происходить). |
| `internal` | Прочая внутренняя ошибка. |

Ошибки не содержат токенов, паролей и ключей. Тексты сервера и хранилища
могут содержать технические детали — хост решает, что показывать
пользователю.

## 7. События

События накапливаются в очереди handle с момента создания (подписка
оформляется до запуска клиента). Формат:

```json
{"@type":"event","event":"message_received","data":{...}}
```

События:

| `event` | `data` |
| --- | --- |
| `connection_state_changed` | `{state}` |
| `session_started` | `{user_id, username}` |
| `session_ended` | `{reason}` |
| `message_received` | объект сообщения |
| `message_updated` | объект сообщения |
| `outbox_changed` | `{local_id, state, error}`; `state` — `pending/sending/sent/failed` |
| `chat_upserted` | объект чата |
| `receipt_updated` | объект receipt |
| `presence_changed` | `{user_id, online}` |
| `call_invited` / `call_accepted` / `call_declined` / `call_ended` | объект звонка |
| `sync_progress` | объект прогресса |
| `resync_performed` | `{up_to_seq}` |
| `e2e_decrypt_failed` | `{chat_id, server_id, device_id}` |
| `warning` | `{message}` |
| `event_lagged` | `{missed}` — очередь переполнилась, события потеряны |

Очередь ограничена 1024 событиями на handle. Если хост не успевает их
забирать, новые события отбрасываются, а при первой возможности возвращается
`event_lagged` с числом потерянных. Это лучше неограниченного роста памяти,
но хост должен либо регулярно вызывать `rchat_receive_json`, либо считать
потери допустимыми.

События — это сигналы, а источник истины — локальное хранилище ядра. После
`event_lagged` и в любом сомнительном случае хосту следует перечитать
`get_chats`/`get_messages`/`get_receipts`, а не полагаться на полноту потока.

События звонков transient и best-effort: они не сохраняются в истории и не
переживают перезапуск. `call_invited` доставляется только callee;
`call_accepted`/`call_declined`/`call_ended` — обоим участникам (в пределах
их активных устройств).

## 8. Безопасность и ограничения

- FFI работает внутри процесса хоста. Хост видит расшифрованные сообщения и
  пароль в момент `login`/`register`; это доверенная граница приложения, а не
  песочница.
- C-ABI не проверяет валидность указателей. Передача освобождённого или
  чужого указателя — неопределённое поведение. Все строки освобождаются только
  через `rchat_free_string`.
- Паники Rust перехватываются; через C-границу unwind не проходит.
- E2E, TLS и проверка сертификатов остаются такими же, как в ядре и
  `rchat-transport-ws`. FFI не добавляет новых сетевых протоколов и не
  ослабляет шифрование.
- `ws://` и `tls_insecure` допустимы только для локальной разработки.
- Медиа/WebRTC в звонках отсутствуют: только invite/accept/decline/end.
- Официальный Python SDK (`bindings/python`) построен на стандартном
  `ctypes` и не добавляет зависимостей; биндинги для других языков пока не
  поставляются. `rchat_api_version()` возвращает версию JSON-схемы (`1`);
  до стабилизации 1.0 несовместимые изменения сопровождаются повышением
  версии.

## 9. Тестирование

Автотесты крейта вызывают C-функции напрямую, поднимают настоящий сервер
(`rchat-testkit`) и проверяют два независимых handle, E2E-сообщение, события
и сигнализацию звонка:

```bash
cargo test -p rchat-ffi
```

Юнит-тесты `rchat-core` проверяют Unicode-инициалы (кириллица, латиница,
CJK, эмодзи, пустые строки) и детерминированность индекса цвета. Тест
`crates/rchat-ffi/tests/schema.rs` проверяет, что `schema/rchat.schema.json`
соответствует Rust-типам и содержит все команды и события:

```bash
cargo test -p rchat-ffi test_schema_is_up_to_date
cargo run -p rchat-ffi --bin export_schema -- --check
```

Ручная проверка C-ABI: соберите библиотеку и пример
`crates/rchat-ffi/examples/ffi_smoke.c` (см. комментарий в файле). Пример
создаёт handle с `auto_start:false`, выполняет `ping` и освобождает строки.

Python SDK также проверяется отдельно:

```bash
cargo build -p rchat-ffi
cargo build --bin rchat-server
python3 -m unittest discover -s bindings/python/tests
```

## 10. Python SDK

Официальный SDK находится в `bindings/python` и использует только
стандартную библиотеку (`ctypes`, `json`, `threading`, `queue`,
`pathlib`, `typing`, `dataclasses`). Подробности и примеры — в
[`bindings/python/README.md`](../bindings/python/README.md).

Пакет предоставляет:

- `rchat.ffi` — низкоуровневые обёртки `rchat_create`, `rchat_send_json`,
  `rchat_receive_json`, `rchat_destroy`, `rchat_version`,
  `rchat_api_version`, `rchat_last_error`. Возвращённые send/receive строки
  копируются и освобождаются через `rchat_free_string`; заимствованные
  строки версии/ошибки не освобождаются.
- `rchat.RChatClient` — высокоуровневый клиент с фоновым потоком событий,
  контекстным менеджером, методами сессии/чатов/сообщений/устройств/звонков
  и моделями-датаклассами `Chat`, `Message`, `Call`, `Session`, `Event`,
  `AvatarInfo`.
- `rchat.RChatError.from_response` — фабрика исключений по кодам из §6.

Минимальный пример:

```python
from rchat import RChatClient

with RChatClient("/tmp/rchat/alice", "wss://chat.example",
                 tls_fingerprint="AA:BB:...") as client:
    client.login("alice", "password")
    chat = client.create_direct_chat("bob", e2e=True)
    client.send_message(chat.chat_id, "привет")

    for event in client.events():
        if event.event == "message_received":
            print(event.data["body"])
```

Аватар доступен и как атрибут, и как элемент словаря:

```python
print(chat.avatar.initials, chat.avatar.color_hex)
print(chat["avatar"]["color_index"])
```

Биндинг ищет библиотеку в `RCHAT_LIB_PATH`, затем в `target/release/` и
`target/debug/` относительно корня репозитория. Примеры:
`bindings/python/examples/smoke_ping.py` (низкоуровневый `ping` без сервера)
и `bindings/python/examples/echo_bot.py`.

`login` нужен только профилю без сохранённого токена: сессия с токеном
восстанавливается при старте (`get_me()`/`get_session()`), а повторный
`login` вернёт `invalid_request: already authenticated` — так же, как
одноимённая команда FFI.

## 11. JSON Schema

Единый источник истины структуры протокола — `schema/rchat.schema.json`
(JSON Schema draft-07, версия API 1). Файл генерируется из Rust-типов
`crates/rchat-ffi/src/schema.rs`, на которых выведен `schemars::JsonSchema`.

Корневая схема — `anyOf` из четырёх вариантов:

- `Command` — все входящие команды §5 с дискриминатором `@type`;
- `OkResponse` — `@type: "ok"`, `request`, `data`;
- `ErrorResponse` — `@type: "error"`, `code`, `message`, `retryable`,
  `server_code?`, `server_code_name?`;
- `Event` — все события §7 (`message_received`, `outbox_changed`,
  `call_invited`, `event_lagged` и т.д.).

В `definitions` находятся общие сущности (`Chat`, `ChatMember`, `Session`,
`Account`, `Message`, `Call`, `Avatar`, `SyncProgress`, `Device`, receipts) и
по одной схеме на команду/событие. Этого достаточно для генерации типов в
Dart/Swift/Kotlin/TypeScript и для валидации JSON в тестах.

Генерация и проверка актуальности:

```bash
# Перезаписать schema/rchat.schema.json.
cargo run -p rchat-ffi --bin export_schema

# Проверить, что файл не устарел (код возврата 1 при расхождении).
cargo run -p rchat-ffi --bin export_schema -- --check

# Тест в составе cargo test.
cargo test -p rchat-ffi test_schema_is_up_to_date
```

Файл коммитится в репозиторий; редактировать его вручную нельзя — при
изменении `schema.rs` запустите экспорт. Тест `test_schema_is_up_to_date`
входит в `cargo test --workspace` и падает, если схема не обновлена.

Пример фрагмента:

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "rchat-ffi JSON protocol",
  "anyOf": [
    {"$ref": "#/definitions/Command"},
    {"$ref": "#/definitions/OkResponse"},
    {"$ref": "#/definitions/ErrorResponse"},
    {"$ref": "#/definitions/Event"}
  ],
  "definitions": {
    "Command": {
      "oneOf": [
        {"type": "object", "required": ["@type"],
         "properties": {"@type": {"type": "string", "enum": ["get_chats"]}}}
      ]
    }
  }
}
```

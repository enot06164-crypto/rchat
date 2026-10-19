# Веб-клиент rchat (этап H)

Официальный веб-интерфейс мессенджера rchat, оформленный в стиле Telegram
WebK: тёмная палитра, пузыри с хвостиками, аватары-инициалы из ядра.

- **SolidJS 1.9 + TypeScript (strict)** — точечная реактивность, без Virtual DOM.
- **Vite 5** — dev-сервер и сборка; **SCSS** — палитра и стили.
- **lucide-solid** — иконки (самолёт, поиск, галочки).
- Протокол — **JSON FFI без изменений** (`docs/ffi.md`): WebSocket-шлюз
  (`bindings/python/rchat/gateway.py`) проксирует `rchat_send_json` и
  ретранслирует события ядра в браузер.

```
apps/web
├── index.html
├── package.json / tsconfig.json / vite.config.ts
└── src
    ├── index.tsx           точка входа (render + dev-хук window.__rchat)
    ├── App.tsx             splash → авторизация → главный экран
    ├── styles/             variables.scss (палитра), global.scss (стили)
    ├── types/rchat.ts      типы Chat/Message/Session/событий (по схеме ядра)
    ├── services/
    │   ├── api.ts          WebSocket RPC (FIFO-сопоставление ответов) + события
    │   └── store.ts        реактивное хранилище SolidJS и все действия
    ├── utils/format.ts     форматирование времени
    └── components/
        ├── Avatar.tsx      круг с color_hex + инициалами из ядра (§5.6 FFI)
        ├── auth/AuthModal.tsx          вход/регистрация
        ├── sidebar/                    Sidebar, ChatList, ChatItem, NewChatModal
        └── chat/                       ChatView, MessageList, MessageBubble, MessageInput
```

## Запуск с нуля (три терминала)

```bash
# 0) один раз: собрать ядро и установить зависимости
cargo build -p rchat-ffi --bin rchat-server
cargo build --bin rchat-server
(cd apps/web && npm install)

# шлюзу нужен внешний пакет websockets (не входит в SDK):
python3 -m venv .venv-gateway
uv pip install --python .venv-gateway/bin/python websockets   # или pip install websockets в venv

# 1) сервер разработки (plaintext только для localhost)
cargo run --bin rchat-server -- \
    --db /tmp/rchat-web/server.sqlite3 \
    --listen 127.0.0.1:9000 \
    --plaintext-insecure --allow-dev-plaintext-chats

# 2) шлюз: один процесс = один профиль (данные профиля ниже)
PYTHONPATH=bindings/python .venv-gateway/bin/python -m rchat.gateway \
    --data-dir /tmp/rchat-web/alice \
    --server ws://127.0.0.1:9000 \
    --port 8080

# 3) веб-клиент (Vite проксирует /ws на шлюз)
cd apps/web && npm run dev          # http://localhost:5173
```

Для сервера с TLS (`wss://`, самоподписанный сертификат) шлюз принимает
`--tls-fingerprint` или `--tls-insecure` (Как в CLI; вне dev TLS обязателен).

## Сборка и «продакшн-превью»

```bash
cd apps/web
npm run build            # tsc --noEmit + vite build → dist/
npm run preview          # http://localhost:4173, /ws проксируется на шлюз
```

## Что уже работает

- вход/регистрация пользователя (AuthModal: вкладки Вход/Регистрация);
- список чатов с цветными аватарами-инициалами из ядра, поиск по названию,
  вкладки «Все»/«Непрочитанные»;
- создание личного чата по username (иконка карандаша рядом с поиском в
  шапке сайдбара, модальное окно);
- лента и ввод — центральная колонна `max-width: 720px` как в Telegram WebK;
  фон чата — фирменный паттерн (SVG, opacity 0.05) поверх радиального
  градиента; композер — «парящая капсула» (эмодзи/скрепка слева, круглая
  синяя кнопка отправки справа);
- шапка чата 56px: аватар, имя, подзаголовок «в сети» / «был(а) недавно»
  (когда приходило `presence_changed`) или «E2E защита»;
- мгновенная лента: оптимистичная вставка, статусы (часы → галочка →
  двойная галочка после прочтения собеседником, восклицание при ошибке),
  автоскролл, Enter — отправка, Shift+Enter — перенос строки;
- события ядра применяются реактивно: connection_state_changed, chat_upserted,
  message_received/updated, outbox_changed, receipt_updated, presence_changed;
  после `event_lagged` хранилище перечитывается из ядра;
- кнопка «Выйти» (`logout`), индикатор состояния соединения.

## Понятные ограничения v1 (что НЕ входит в этап H)

- Счётчик «непрочитанных» — **локальный** и не переживает перезагрузку
  страницы; отметка `mark_read` отправляется при открытом чате, чтобы
  собеседник видел «прочитано».
- `presence_changed` применяется только как подзаголовок «в сети» /
  «был(а) недавно»; пока событие не приходило в этой сессии — показываем
  «E2E защита» (честный статический факт).
- Иконки в шапке чата (поиск по чату, звонок, меню) и эмодзи/скрепка в
  композере — **визуальные заглушки** v1: функциональность появится на
  этапе звонков/файлов.
- Зеркальное echo собственного сообщения от ядра (`local_id` вида `s:<id>`,
  body null) дедуплицируется по `server_id`; история — стартовой страницей
  `get_messages(limit=100)` (пагинация «вверх» — не реализована).
- Один шлюз-процесс = один профиль. Несколько вкладок браузера делят один
  и тот же профиль (события и ответы рассылаются всем сокетам).

## Отладка

- `window.__rchat.state` — снимок reactive-хранилища в DevTools (только
  dev-сервер Vite).
- Deep-link `http://localhost:5173/?chat=<chat_id>` сразу открывает чат
  (для смоков и закладок).
- Протокольный смок шлюза (два пользователя, E2E-сообщение): если есть
  `websockets` в окружении — запустите `apps/web/scripts/smoke_gateway.py`
  (перед этим `cargo build --bin rchat-server`).

/**
 * Реактивное хранилище SolidJS (createStore) и все действия приложения.
 *
 * Источник истины — локальное хранилище ядра: список чатов берётся из
 * `get_chats`, история — `get_messages`, receipts — `get_receipts`.  События
 * (§7 docs/ffi.md) обновляют копии данных мгновенно; после `event_lagged`
 * хранилище перечитывается с ядра.
 *
 * Состояние разделено по аккаунтам: у каждого профиля шлюза свои чаты,
 * сообщения и сессия, поэтому данные лежат в `state.accounts[profile]`.
 * Общими остаются только список профилей и признаки соединения.
 */

import { createStore, produce } from "solid-js/store";
import {
  apiFor,
  chatApiFor,
  closeProfile,
  listProfiles,
} from "./api";
import {
  classifyError,
  classifyOutboxFailure,
  noticeKey,
  noticeText,
  type ErrorNotice,
} from "./errors";
import type {
  AttachmentInfo,
  AttachmentUpload,
  Chat,
  ChatMember,
  ChatPrivacy,
  ConnectionState,
  Message,
  MessageStatus,
  RchatEvent,
  Receipt,
  SearchHit,
  Session,
  UserSummary,
} from "../types/rchat";

/** Состояние одного аккаунта (профиля шлюза). */
export interface AccountState {
  session: Session | null;
  /** Чаты, отсортированные по `updated_at_ms` по убыванию. */
  chats: Chat[];
  /** chat_id → сообщения по возрастанию `created_at_ms`. */
  messages: Record<string, Message[]>;
  /** chat_id → receipts (заменяются целиком при получении). */
  receipts: Record<string, Receipt[]>;
  /** chat_id → (user_id → участник); заполняется для групповых чатов. */
  members: Record<string, Record<string, ChatMember>>;
  activeChatId: string | null;
  /** Локальные счётчики «непрочитанного» (см. README apps/web). */
  unread: Record<string, number>;
  /** Сообщение, на которое отвечаем из композера (гасится после отправки). */
  replyTo: Message | null;
  booted: boolean;
  busy: boolean;
  authError: ErrorNotice | null;
  /** Ошибка отправки в чате `sendErrorChatId` (в других чатах не показывается). */
  sendError: ErrorNotice | null;
  sendErrorChatId: string | null;
  newChatError: ErrorNotice | null;
  /** Ошибка загрузки истории (с действием «повторить»). */
  historyError: ErrorNotice | null;
  /** Исходящий запрос на приватный режим: ждём подтверждения собеседника. */
  privateModeRequested: boolean;
  /** Входящее предложение включить приватный режим (от собеседника). */
  privateModeOffer: {
    chat_id: string;
    requester_username: string;
    requester_display_name: string;
  } | null;
  /** Режим чатов по умолчанию: облачный (`cloud`) или приватный (`private`). */
  defaultChatMode: ChatPrivacy;
  /** Найденные сообщения (поиск по истории; только для облачного режима). */
  searchHits: SearchHit[];
  /** Идёт ли поиск по сообщениям. */
  searchBusy: boolean;
  /** Присутствие собеседников (user_id → online). */
  presence: Record<string, boolean>;
}

export interface AppState {
  /** Связь браузера с шлюзом (UI-уровень, к состояниям FFI отношения не имеет). */
  linkUp: boolean;
  /** Состояние соединения ядра с сервером (§5.1). */
  connection: ConnectionState;
  /** Профили (аккаунты), известные шлюзу. */
  profiles: string[];
  /** Аккаунт, показанный в интерфейсе. */
  activeProfile: string;
  /** Открыта ли карточка «Добавить аккаунт». */
  addAccountOpen: boolean;
  /** Состояние аккаунтов по профилям. */
  accounts: Record<string, AccountState>;
}

function emptyAccount(): AccountState {
  return {
    session: null,
    chats: [],
    messages: {},
    receipts: {},
    members: {},
    activeChatId: null,
    unread: {},
    replyTo: null,
    booted: false,
    busy: false,
    authError: null,
    sendError: null,
    sendErrorChatId: null,
    newChatError: null,
    historyError: null,
    privateModeRequested: false,
    privateModeOffer: null,
    defaultChatMode: "cloud",
    searchHits: [],
    searchBusy: false,
    presence: {},
  };
}

const INITIAL_STATE: AppState = {
  linkUp: false,
  connection: "disconnected",
  profiles: ["default"],
  activeProfile: "default",
  addAccountOpen: false,
  accounts: {},
};

export const [state, setState] = createStore<AppState>(INITIAL_STATE);

// ---------------------------------------------------------------------------
// Вспомогательные функции

/** Состояние аккаунта; создаётся при первом обращении. */
export function accountState(profile: string): AccountState {
  if (state.accounts[profile] === undefined) {
    setState(
      produce((s) => {
        s.accounts[profile] = emptyAccount();
      })
    );
  }
  return state.accounts[profile];
}

/**
 * Ставит уведомление об ошибке.  То же самое уведомление не ставится
 * повторно — иначе получится поток одинаковых сообщений.
 */
function setNotice(
  profile: string,
  field: "authError" | "newChatError" | "historyError",
  notice: ErrorNotice | null
): void {
  const current = accountState(profile)[field];
  if (
    notice !== null &&
    current !== null &&
    noticeKey(current) === noticeKey(notice)
  ) {
    return;
  }
  setState("accounts", profile, field, notice);
}

/**
 * Ошибка отправки — с привязкой к чату: в других чатах она не показывается,
 * и устаревший ответ не выдаётся за ошибку другого аккаунта.
 */
function setSendError(
  profile: string,
  chatId: string | null,
  notice: ErrorNotice | null
): void {
  const account = accountState(profile);
  if (
    notice !== null &&
    account.sendError !== null &&
    account.sendErrorChatId === chatId &&
    noticeKey(account.sendError) === noticeKey(notice)
  ) {
    return;
  }
  setState(
    "accounts",
    profile,
    produce((s) => {
      s.sendError = notice;
      s.sendErrorChatId = chatId;
    })
  );
}

/** Чат, которому принадлежит сообщение с таким локальным id. */
function chatIdOfLocal(profile: string, localId: string): string | null {
  const messages = accountState(profile).messages;
  for (const list of Object.values(messages)) {
    const found = list.find((message) => message.local_id === localId);
    if (found !== undefined) return found.chat_id;
  }
  return null;
}

function byCreatedDesc(a: Chat, b: Chat): number {
  return b.updated_at_ms - a.updated_at_ms || b.created_at_ms - a.created_at_ms;
}

function sortMessages(list: Message[]): Message[] {
  return [...list].sort((a, b) => {
    const delta = a.created_at_ms - b.created_at_ms;
    return delta !== 0 ? delta : a.local_id.localeCompare(b.local_id);
  });
}

function upsertSorted(list: Message[], message: Message): Message[] {
  const rest = list.filter((m) => m.local_id !== message.local_id);
  return sortMessages([...rest, message]);
}

function upsertChat(profile: string, chat: Chat): void {
  setState("accounts", profile, "chats", (prev) => {
    const rest = prev.filter((c) => c.chat_id !== chat.chat_id);
    // Событие по чату приходит без превью — сохраняем прежнее, чтобы список
    // не «слетал» на «Нет сообщений».
    const previous = prev.find((c) => c.chat_id === chat.chat_id);
    const merged =
      previous !== undefined &&
      (chat.last_message === null || chat.last_message === undefined)
        ? { ...chat, last_message: previous.last_message ?? null }
        : chat;
    return [...rest, merged].sort(byCreatedDesc);
  });
}

function mergeMessages(
  profile: string,
  chatId: string,
  fetched: Message[]
): void {
  setState("accounts", profile, "messages", chatId, (prev = []) => {
    const map = new Map<string, Message>();
    for (const message of fetched) map.set(message.local_id, message);
    for (const message of prev)
      if (!map.has(message.local_id)) map.set(message.local_id, message);
    return sortMessages([...map.values()]);
  });
}

function mergeSingleMessage(profile: string, message: Message): void {
  const existing = accountState(profile).messages[message.chat_id] ?? [];
  // Ядро зеркалит отправителю его сообщение (для других устройств) с
  // local_id вида "s:<server_id>".  У нас уже есть копия этого сообщения —
  // обновляем её на месте, а не добавляем вторую.
  //
  // Сверяемся по server_id, а не по пустому тексту: в приватном чате ядро
  // присылает эхо без текста (у отправителя нет своей копии ciphertext),
  // а в облачном — уже с текстом (сервер его расшифровал).
  if (message.server_id !== null) {
    const same = existing.find((m) => m.server_id === message.server_id);
    if (same !== undefined && same.local_id !== message.local_id) {
      setState("accounts", profile, "messages", message.chat_id, (prev = []) =>
        prev.map((m) =>
          m.local_id === same.local_id
            ? {
                ...message,
                // local_id сохраняем: по нему приходит статус отправки.
                local_id: m.local_id,
                // В приватном чате эхо приходит без текста (у отправителя
                // нет своей копии ciphertext) — свой текст не теряем.
                body: message.body ?? m.body,
                attachment: message.attachment ?? m.attachment,
                status: m.status ?? "sent",
              }
            : m
        )
      );
      return;
    }
  }
  setState("accounts", profile, "messages", message.chat_id, (prev = []) =>
    upsertSorted(prev, message)
  );
  const known = accountState(profile).chats.some(
    (c) => c.chat_id === message.chat_id
  );
  if (!known) void refreshChats(profile);
}

function setOutboxState(
  profile: string,
  localId: string,
  status: MessageStatus,
  error: string | null
): void {
  const account = accountState(profile);
  for (const chatId of Object.keys(account.messages)) {
    const list = account.messages[chatId] ?? [];
    if (!list.some((m) => m.local_id === localId)) continue;
    setState(
      "accounts",
      profile,
      "messages",
      chatId,
      (prev = []) => prev.map((m) => (m.local_id === localId ? { ...m, status } : m))
    );
    if (status === "failed" && chatId === account.activeChatId) {
      setSendError(profile, chatId, {
        kind: "unknown",
        title: "Сообщение не отправлено",
        message:
          error ?? "Сообщение осталось в очереди — можно повторить отправку.",
        retryable: true,
        operation: "send",
      });
    }
    return;
  }
}

/** Максимальный `read_up_to` по всем собеседникам чата («прочитано»). */
export function readUpToFor(profile: string, chatId: string): number {
  const receipts = accountState(profile).receipts[chatId] ?? [];
  return receipts.reduce((max, r) => Math.max(max, r.read_up_to), 0);
}

export function connectionLabel(connection: ConnectionState): string {
  switch (connection) {
    case "connected":
      return "в сети";
    case "connecting":
      return "подключение…";
    case "reconnecting":
      return "переподключение…";
    case "unauthenticated":
      return "требуется вход";
    case "disconnected":
      return "нет соединения";
  }
}

// ---------------------------------------------------------------------------
// Boot и события

const booting = new Set<string>();

/**
 * Читает состояние ядра аккаунта.  Вызывается после каждого успешного
 * подключения WebSocket (см. App.tsx); локальные данные доступны и до
 * установки соединения с сервером.
 */
export async function boot(profile: string): Promise<void> {
  if (booting.has(profile)) return;
  booting.add(profile);
  accountState(profile); // слот аккаунта должен существовать до setState
  const chatApi = chatApiFor(profile);
  try {
    const connection = await chatApi.getConnectionState();
    setState("connection", connection.state as ConnectionState);
    const session = (await chatApi.getSession()) as Session | null;
    setState("accounts", profile, "session", session);
    await loadSettings(profile);
    if (session !== null) {
      const chats = (await chatApi.getChats()) as Chat[];
      setState("accounts", profile, "chats", chats.sort(byCreatedDesc));
      // Deep-link для смоков и закладок: /?chat=<chat_id> открывает чат сразу
      const want = new URLSearchParams(window.location.search).get("chat");
      if (want !== null && chats.some((c) => c.chat_id === want)) {
        void openChat(profile, want);
      }
    }
    setState("accounts", profile, "booted", true);
  } catch {
    // шлюз ещё не готов — App.tsx повторит boot после реконнекта
    setState("accounts", profile, "booted", false);
  } finally {
    booting.delete(profile);
  }
}

/** Ключ профиля в sessionStorage: профиль — на вкладку, поэтому в двух
 * вкладках можно держать разные аккаунты (как Telegram Web A/B). */
const PROFILE_STORAGE_KEY = "rchat.profile";

/** Профиль вкладки: из адреса (`?profile=`), иначе из sessionStorage. */
export function initialProfile(): string {
  const fromUrl = new URLSearchParams(window.location.search).get("profile");
  if (fromUrl !== null && fromUrl.trim() !== "") return fromUrl.trim();
  try {
    const value = window.sessionStorage.getItem(PROFILE_STORAGE_KEY);
    return value !== null && value.trim() !== "" ? value.trim() : "default";
  } catch {
    return "default";
  }
}

/** Запоминает профиль вкладки. */
function rememberProfile(profile: string): void {
  try {
    window.sessionStorage.setItem(PROFILE_STORAGE_KEY, profile);
  } catch {
    /* sessionStorage может быть недоступен — не критично */
  }
}

/** Читает список аккаунтов (профилей) шлюза. */
export async function loadProfiles(): Promise<void> {
  const profiles = await listProfiles();
  if (profiles.length > 0) {
    setState("profiles", profiles);
  }
}

/**
 * Переключает активный аккаунт (профиль): соединения всех аккаунтов
 * остаются открыты, меняется только то, что показано.
 */
export function activateProfile(profile: string): void {
  if (profile.trim() === "") return;
  setState("activeProfile", profile);
  rememberProfile(profile);
}

/**
 * Переключает аккаунт (профиль): соединение открывается, если его ещё нет,
 * состояние аккаунта своё.  Все аккаунты остаются подключёнными — как
 * вкладки в Telegram Desktop.
 */
export async function switchProfile(profile: string): Promise<void> {
  const name = profile.trim();
  if (name === "") return;
  rememberProfile(name);
  activateProfile(name);
  connectProfile(name);
  await loadProfiles();
  await boot(name);
}

/** Открыть/закрыть карточку «Добавить аккаунт». */
export function setAddAccountOpen(open: boolean): void {
  setState("addAccountOpen", open);
}

/**
 * Имя профиля для нового аккаунта — по логину пользователя (как в
 * Telegram: профиль = аккаунт).  Недопустимые символы заменяем.
 */
export function profileNameFor(username: string): string {
  const clean = username
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9_-]/g, "_")
    .slice(0, 32);
  return clean === "" ? "account" : clean;
}

/**
 * Ждёт, пока ядро аккаунта соединится с сервером: без этого вход и
 * регистрация отклоняются («шлюз не подключён»).  Возвращает true, если
 * соединение установлено.
 */
export async function waitConnected(
  profile: string,
  timeoutMs = 15000
): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const state = await chatApiFor(profile).getConnectionState();
      // «unauthenticated» — транспорт готов, но ещё нет сессии: для входа
      // этого достаточно.
      if (state.state === "connected" || state.state === "unauthenticated") {
        return true;
      }
    } catch {
      /* ещё не готов — пробуем снова */
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  return false;
}

/**
 * Добавляет аккаунт: профиль с именем пользователя подключается, в нём
 * выполняется вход (или регистрация), и аккаунт становится активным.
 * Возвращает ошибку текстом или `null` при успехе.
 */
export async function addAccount(
  username: string,
  password: string,
  displayName: string,
  create: boolean
): Promise<string | null> {
  const name = profileNameFor(username);
  if (!state.profiles.includes(name)) {
    setState("profiles", [...state.profiles, name]);
  }
  connectProfile(name);
  await boot(name);
  // Ядро должно успеть соединиться с сервером, иначе вход отклоняется.
  if (!(await waitConnected(name))) {
    return "не удалось подключиться к серверу";
  }
  const ok = create
    ? await register(name, username, displayName, password)
    : await login(name, username, password);
  if (!ok) {
    const notice = accountState(name).authError;
    return notice === null ? "не удалось войти" : noticeText(notice);
  }
  activateProfile(name);
  await loadProfiles();
  return null;
}

/** Имя нового профиля: короткое и уникальное в пределах шлюза. */
export function nextProfileName(existing: string[]): string {
  let index = existing.length + 1;
  let name = `account${index}`;
  while (existing.includes(name)) {
    index += 1;
    name = `account${index}`;
  }
  return name;
}

/**
 * Настройки клиента: режим чатов по умолчанию (облачный или приватный).
 */
export async function loadSettings(profile: string): Promise<void> {
  accountState(profile);
  try {
    const settings = await chatApiFor(profile).getSettings();
    setState(
      "accounts",
      profile,
      "defaultChatMode",
      settings.default_chat_privacy === "private" ? "private" : "cloud"
    );
  } catch {
    /* шлюз не готов — останется текущее значение */
  }
}

/** Меняет режим чатов по умолчанию (применяется к новым чатам). */
export async function setDefaultChatMode(
  profile: string,
  mode: ChatPrivacy
): Promise<void> {
  setState("accounts", profile, "defaultChatMode", mode);
  try {
    await chatApiFor(profile).setDefaultChatMode(mode);
  } catch (error) {
    setState(
      "accounts",
      profile,
      "defaultChatMode",
      mode === "cloud" ? "private" : "cloud"
    );
    console.warn("rchat: set default chat mode", error);
  }
}

/**
 * Поиск по сообщениям аккаунта.  Работает в облачном режиме: сервер
 * расшифровывает содержимое ключом чата и ищет по нему.  В приватных чатах
 * содержимое серверу недоступно, поэтому в выдаче их нет.
 */
export async function searchMessages(
  profile: string,
  query: string
): Promise<void> {
  accountState(profile);
  const trimmed = query.trim();
  if (trimmed === "") {
    setState(
      "accounts",
      profile,
      produce((s) => {
        s.searchHits = [];
        s.searchBusy = false;
      })
    );
    return;
  }
  setState("accounts", profile, "searchBusy", true);
  try {
    const result = await chatApiFor(profile).searchMessages(trimmed);
    setState("accounts", profile, "searchHits", result.hits ?? []);
  } catch (error) {
    console.warn("rchat: search messages", error);
    setState("accounts", profile, "searchHits", []);
  } finally {
    setState("accounts", profile, "searchBusy", false);
  }
}

export async function refreshChats(profile: string): Promise<void> {
  accountState(profile);
  try {
    const chats = (await chatApiFor(profile).getChats()) as Chat[];
    setState("accounts", profile, "chats", chats.sort(byCreatedDesc));
  } catch {
    /* переподключение — состояние соединения придёт событием */
  }
}

/** Перечитывает состояние соединения ядра (после auth-переходов). */
async function updateConnection(profile: string): Promise<void> {
  try {
    const connection = await chatApiFor(profile).getConnectionState();
    setState("connection", connection.state as ConnectionState);
  } catch {
    /* без соединения покажем прежнее состояние */
  }
}

/** Применяет одно событие ядра к хранилищу аккаунта. */
export function applyEvent(profile: string, event: RchatEvent): void {
  switch (event.event) {
    case "connection_state_changed": {
      const data = event.data as { state: ConnectionState };
      setState("connection", data.state);
      return;
    }
    case "session_started": {
      // полный Session (с аватаром) берём из ядра, событие — триггер
      void (async () => {
        try {
          const session = (await chatApiFor(profile).getSession()) as Session | null;
          setState("accounts", profile, "session", session);
          if (session !== null) void refreshChats(profile);
        } catch {
          /* проигнорируем: следующий event повторит попытку */
        }
      })();
      return;
    }
    case "session_ended": {
      setState(
        "accounts",
        profile,
        produce((s) => {
          s.session = null;
          s.activeChatId = null;
          s.messages = {};
          s.receipts = {};
          s.members = {};
          s.unread = {};
          s.presence = {};
          s.replyTo = null;
          s.sendError = null;
        })
      );
      return;
    }
    case "chat_upserted": {
      upsertChat(profile, event.data as Chat);
      return;
    }
    case "message_received": {
      const message = event.data as Message;
      mergeSingleMessage(profile, message);
      const account = accountState(profile);
      const meId = account.session?.user_id ?? null;
      if (meId !== null && message.sender_user_id !== meId) {
        if (message.chat_id === account.activeChatId) {
          // чат открыт — сообщение прочитано сразу
          if (message.server_id !== null) {
            void chatApiFor(profile)
              .markRead(message.chat_id)
              .catch(() => undefined);
          }
        } else {
          setState("accounts", profile, "unread", message.chat_id, (prev = 0) => prev + 1);
        }
      }
      return;
    }
    case "message_updated": {
      mergeSingleMessage(profile, event.data as Message);
      return;
    }
    case "outbox_changed": {
      const data = event.data as {
        local_id: string;
        state: Exclude<MessageStatus, "none">;
        error: string | null;
        error_code?: number | null;
      };
      setOutboxState(profile, data.local_id, data.state, data.error);
      if (data.state === "failed") {
        // Сообщение осталось в очереди: предлагаем повторить (без новой
        // копии — повтор возвращает ту же запись в очередь).
        const chatId = chatIdOfLocal(profile, data.local_id);
        setSendError(
          profile,
          chatId,
          classifyOutboxFailure(data.error_code, {
            operation: "send",
            chatId: chatId ?? undefined,
          })
        );
      }
      return;
    }
    case "private_mode_requested": {
      // Собеседник предлагает включить приватный режим: показываем окно
      // с выбором «Принять» / «Отклонить».
      const data = event.data as {
        chat_id: string;
        requester_username: string;
        requester_display_name: string;
      };
      setState(
        "accounts",
        profile,
        "privateModeOffer",
        {
          chat_id: data.chat_id,
          requester_username: data.requester_username,
          requester_display_name: data.requester_display_name,
        }
      );
      return;
    }
    case "private_mode_cancelled": {
      // Наш запрос отозван — ожидание снимается.
      setState("accounts", profile, "privateModeRequested", false);
      return;
    }
    case "private_mode_resolved": {
      // Запрос разрешён или отклонён — ожидание снимается.
      setState("accounts", profile, "privateModeRequested", false);
      return;
    }
    case "receipt_updated": {
      const receipt = event.data as Receipt;
      setState("accounts", profile, "receipts", receipt.chat_id, (prev = []) => {
        const rest = prev.filter((r) => r.user_id !== receipt.user_id);
        return [...rest, receipt];
      });
      return;
    }
    case "event_lagged": {
      const data = event.data as { missed: number };
      console.warn(
        `rchat: очередь событий переполнилась, потеряно событий: ${data.missed}; перечитываем данные`
      );
      void refreshChats(profile);
      const active = accountState(profile).activeChatId;
      if (active !== null) void loadChatHistory(profile, active);
      return;
    }
    case "presence_changed": {
      // статус собеседника для подзаголовка шапки («в сети» / «был(а) недавно»);
      // до первого события показываем только признак шифрования
      const data = event.data as { user_id: string; online: boolean };
      setState("accounts", profile, "presence", data.user_id, data.online);
      return;
    }
    case "warning": {
      const data = event.data as { message: string };
      console.warn("rchat warning:", data.message);
      return;
    }
    default:
      // sync_progress, resync_performed и call_* событий
      // v1 веб-UI не отображает: расширенная логика появится на этапе звонков.
      return;
  }
}

// ---------------------------------------------------------------------------
// Действия пользователя

export async function openChat(profile: string, chatId: string): Promise<void> {
  setState(
    "accounts",
    profile,
    produce((s) => {
      s.activeChatId = chatId;
      s.sendError = null;
      s.replyTo = null;
    })
  );
  setState("accounts", profile, "unread", chatId, 0);
  void loadChatHistory(profile, chatId);
}

/** Начать ответ на сообщение (реплай в композере, FFI `reply_to_server_id`). */
export function setReplyTo(profile: string, message: Message): void {
  setState("accounts", profile, "replyTo", message);
}

export function clearReply(profile: string): void {
  setState("accounts", profile, "replyTo", null);
}

/** Скрывает ошибку отправки в чате. */
export function clearSendError(profile: string, chatId: string): void {
  const account = accountState(profile);
  if (account.sendErrorChatId !== chatId) return;
  setSendError(profile, chatId, null);
}

/**
 * Удаление сообщения (FFI `delete_message`). Локально помечаем как удалённое
 * сразу; подтверждение придёт событием `message_updated` идемпотентно.
 */
export async function deleteMessage(
  profile: string,
  chatId: string,
  serverId: number
): Promise<void> {
  try {
    await chatApiFor(profile).deleteMessage(chatId, serverId);
    setState("accounts", profile, "messages", chatId, (prev = []) =>
      prev.map((m) =>
        m.server_id === serverId && m.deleted_at_ms === null
          ? { ...m, deleted_at_ms: Date.now(), body: null }
          : m
      )
    );
  } catch (error) {
    console.warn("rchat: delete_message", error);
  }
}

/** Гарантирует, что состав группового чата загружен (для шторки «Информация»). */
export async function ensureMembers(
  profile: string,
  chatId: string
): Promise<void> {
  if (accountState(profile).members[chatId] !== undefined) return;
  try {
    const members = (await chatApiFor(profile).getChatMembers(chatId)) as ChatMember[];
    setState(
      "accounts",
      profile,
      "members",
      chatId,
      Object.fromEntries(members.map((m) => [m.user_id, m]))
    );
  } catch {
    /* шторка покажет пустой список */
  }
}

/**
 * Общие группы с пользователем: пересечение списка групп и их участников.
 * Данных ядра достаточно (`get_chats` + `get_chat_members`): отдельной
 * команды «общие чаты» в FFI нет.
 */
export async function loadCommonGroups(
  profile: string,
  peerUserId: string
): Promise<Chat[]> {
  const groups = accountState(profile).chats.filter((chat) => chat.kind === "group");
  const common: Chat[] = [];
  for (const chat of groups) {
    await ensureMembers(profile, chat.chat_id);
    const members = accountState(profile).members[chat.chat_id] ?? {};
    if (peerUserId in members) common.push(chat);
  }
  return common;
}

async function loadChatHistory(
  profile: string,
  chatId: string
): Promise<void> {
  const chatApi = chatApiFor(profile);
  const job = (async () => {
    const chat = accountState(profile).chats.find((c) => c.chat_id === chatId);
    const requests: Promise<void>[] = [];
    setNotice(profile, "historyError", null);

    requests.push(
      (async () => {
        const fresh = (await chatApi.getMessages(chatId)) as Message[];
        mergeMessages(profile, chatId, fresh.slice().reverse()); // ядро: новые первыми
      })()
    );

    requests.push(
      (async () => {
        const receipts = (await chatApi.getReceipts(chatId)) as Receipt[];
        setState("accounts", profile, "receipts", chatId, receipts);
      })()
    );

    if (chat !== undefined && chat.kind === "group") {
      requests.push(
        (async () => {
          const members = (await chatApi.getChatMembers(chatId)) as ChatMember[];
          setState(
            "accounts",
            profile,
            "members",
            chatId,
            Object.fromEntries(members.map((m) => [m.user_id, m]))
          );
        })()
      );
    }

    const results = await Promise.allSettled(requests);
    for (const result of results) {
      if (result.status === "rejected") {
        // Ошибка чтения — понятное уведомление с действием «повторить».
        const reason = result.reason;
        console.warn(reason);
        setNotice(
          profile,
          "historyError",
          classifyError(reason, { operation: "history", chatId })
        );
      }
    }
    try {
      await chatApi.markRead(chatId);
    } catch {
      /* отметка о прочтении не критична для UI */
    }
  })();
  await job;
}

/**
 * Ставит сообщение в очередь отправки.
 *
 * Возвращает `true`, если сообщение ушло в очередь, и `false`, если отправка
 * не началась (ошибка до постановки) — тогда ввод пользователя сохраняется.
 */
export async function sendCurrentText(
  profile: string,
  text: string,
  attachments: AttachmentUpload[] = []
): Promise<boolean> {
  const account = accountState(profile);
  const chatId = account.activeChatId;
  const session = account.session;
  const trimmed = text.trim();
  if (chatId === null || session === null) return false;
  if (trimmed === "" && attachments.length === 0) return false;

  setState("accounts", profile, "sendError", null);
  const replyTo = account.replyTo;
  // Альбом: несколько вложений делят общий grouped_id.
  const groupedId =
    attachments.length > 1
      ? `g-${Date.now()}-${Math.random().toString(16).slice(2)}`
      : null;
  const now = Date.now();
  const optimisticMessages: Message[] =
    attachments.length === 0
      ? [
          {
            local_id: `web-${now}-${Math.random().toString(16).slice(2)}`,
            server_id: null,
            chat_id: chatId,
            sender_user_id: session.user_id,
            sender_device_id: "",
            content_kind: "text",
            body: trimmed,
            attachment: null,
            grouped_id: groupedId,
            reply_to_server_id: replyTo !== null ? replyTo.server_id : null,
            status: "pending",
            created_at_ms: now,
            edited_at_ms: null,
            deleted_at_ms: null,
          },
        ]
      : attachments.map((attachment, index) => ({
          local_id: `web-${now}-${index}-${Math.random().toString(16).slice(2)}`,
          server_id: null,
          chat_id: chatId,
          sender_user_id: session.user_id,
          sender_device_id: "",
          content_kind: attachment.kind,
          body:
            trimmed === "" || index + 1 < attachments.length ? null : trimmed,
          attachment: {
            kind: attachment.kind,
            name: attachment.name,
            mime: attachment.mime,
            size: attachment.size,
            thumb:
              attachment.thumb_base64 === undefined
                ? undefined
                : {
                    kind: "photo",
                    name: "poster.jpg",
                    mime: attachment.thumb_mime ?? "image/jpeg",
                    size: attachment.size,
                  },
            waveform: attachment.waveform_base64 ?? null,
          } satisfies AttachmentInfo,
          grouped_id: groupedId,
          reply_to_server_id: replyTo !== null ? replyTo.server_id : null,
          status: "pending",
          created_at_ms: now + index,
          edited_at_ms: null,
          deleted_at_ms: null,
        }));
  setState("accounts", profile, "messages", chatId, (prev = []) =>
    optimisticMessages.reduce((acc, message) => upsertSorted(acc, message), prev)
  );

  try {
    const result = (await chatApiFor(profile).sendMessage(
      chatId,
      trimmed,
      replyTo !== null ? replyTo.server_id : null,
      attachments
    )) as { local_id?: string; local_ids?: string[] };
    setState(
      "accounts",
      profile,
      "messages",
      chatId,
      (prev = []) =>
        prev.map((m) => {
          const index = optimisticMessages.findIndex(
            (item) => item.local_id === m.local_id
          );
          if (index === -1) return m;
          const localId =
            result.local_ids?.[index] ?? result.local_id ?? m.local_id;
          return {
            ...m,
            local_id: localId,
            status: "sending" as MessageStatus,
          };
        })
    );
    clearReply(profile);
  } catch (error) {
    setState(
      "accounts",
      profile,
      "messages",
      chatId,
      (prev = []) =>
        prev.map((m) =>
          optimisticMessages.some((item) => item.local_id === m.local_id)
            ? { ...m, status: "failed" as MessageStatus }
            : m
        )
    );
    setSendError(
      profile,
      chatId,
      classifyError(error, { operation: "send", chatId })
    );
    return false;
  }
  return true;
}

/**
 * Повторяет отправку упавшего сообщения: в очередь возвращается та же
 * запись (тот же ключ идемпотентности), поэтому новой копии не будет.
 */
export async function retryMessage(
  profile: string,
  chatId: string,
  localId: string
): Promise<void> {
  try {
    const result = (await chatApiFor(profile).retryMessage(localId)) as {
      requeued: boolean;
    };
    if (!result.requeued) {
      setSendError(profile, chatId, {
        kind: "unknown",
        title: "Повтор не нужен",
        message: "Сообщение уже отправлено или больше не в очереди.",
        retryable: false,
        operation: "send",
      });
      return;
    }
    setSendError(profile, chatId, null);
  } catch (error) {
    setSendError(
      profile,
      chatId,
      classifyError(error, { operation: "send", chatId })
    );
  }
}

/** Повторяет загрузку истории чата. */
export async function retryHistory(
  profile: string,
  chatId: string
): Promise<void> {
  setNotice(profile, "historyError", null);
  await loadChatHistory(profile, chatId);
}

/**
 * Просит собеседника перевести личный чат в приватный режим (E2E).
 * Пока ждём ответа — в интерфейсе висит модалка с ожиданием.
 */
export async function requestPrivateMode(
  profile: string,
  chatId: string
): Promise<void> {
  try {
    const pending = await chatApiFor(profile).requestPrivateMode(chatId);
    setState("accounts", profile, "privateModeRequested", pending.pending);
  } catch (error) {
    setNotice(
      profile,
      "historyError",
      classifyError(error, { operation: "chat" })
    );
  }
}

/** Отзывает свой запрос на приватный режим. */
export async function cancelPrivateMode(
  profile: string,
  chatId: string
): Promise<void> {
  try {
    await chatApiFor(profile).cancelPrivateMode(chatId);
  } catch (error) {
    console.warn("rchat: cancel private mode", error);
  }
  setState("accounts", profile, "privateModeRequested", false);
}

/** Отвечает на предложение: принять (чат переходит в E2E) или отклонить. */
export async function resolvePrivateMode(
  profile: string,
  chatId: string,
  accept: boolean
): Promise<void> {
  try {
    await chatApiFor(profile).resolvePrivateMode(chatId, accept);
  } catch (error) {
    setNotice(
      profile,
      "historyError",
      classifyError(error, { operation: "chat" })
    );
  }
  setState("accounts", profile, "privateModeOffer", null);
}

/**
 * Ищет пользователей по префиксу username (для выбора собеседника).
 * Список пользователей знает только сервер, поэтому запрос уходит к нему.
 */
export async function searchUsers(
  profile: string,
  query: string
): Promise<UserSummary[]> {
  const trimmed = query.trim();
  if (trimmed.length < 2) return [];
  try {
    const result = await chatApiFor(profile).searchUsers(trimmed, 20);
    return result.users ?? [];
  } catch (error) {
    console.warn("rchat: search users", error);
    return [];
  }
}

export async function createDirectChat(
  profile: string,
  usernameRaw: string,
  mode?: ChatPrivacy
): Promise<Chat | null> {
  accountState(profile);
  const username = usernameRaw.trim().replace(/^@+/, "");
  setNotice(profile, "newChatError", null);
  if (username === "") {
    setNotice(profile, "newChatError", {
      kind: "invalid_input",
      title: "Нужен username",
      message: "Введите имя пользователя собеседника.",
      retryable: false,
      operation: "chat",
    });
    return null;
  }
  try {
    const chat = (await chatApiFor(profile).createDirectChat(username, mode)) as Chat;
    upsertChat(profile, chat);
    return chat;
  } catch (error) {
    setNotice(
      profile,
      "newChatError",
      classifyError(error, { operation: "chat" })
    );
    return null;
  }
}

export async function login(
  profile: string,
  username: string,
  password: string
): Promise<boolean> {
  accountState(profile);
  setState(
    "accounts",
    profile,
    produce((s) => {
      s.authError = null;
      s.busy = true;
    })
  );
  try {
    const session = (await chatApiFor(profile).login(username, password)) as Session;
    setState("accounts", profile, "session", session);
    void refreshChats(profile);
    void updateConnection(profile);
    return true;
  } catch (error) {
    setNotice(
      profile,
      "authError",
      classifyError(error, { operation: "login" })
    );
    return false;
  } finally {
    setState("accounts", profile, "busy", false);
  }
}

export async function register(
  profile: string,
  username: string,
  displayName: string,
  password: string
): Promise<boolean> {
  accountState(profile);
  setState("accounts", profile, { authError: null, busy: true });
  try {
    const session = (await chatApiFor(profile).register(
      username,
      displayName,
      password
    )) as Session;
    setState("accounts", profile, "session", session);
    void refreshChats(profile);
    void updateConnection(profile);
    return true;
  } catch (error) {
    setNotice(
      profile,
      "authError",
      classifyError(error, { operation: "register" })
    );
    return false;
  } finally {
    setState("accounts", profile, "busy", false);
  }
}

export async function logout(profile: string): Promise<void> {
  accountState(profile);
  try {
    await chatApiFor(profile).logout();
  } catch (error) {
    // даже при ошибке (например not_connected) сеанс сбрасываем локально —
    // событие session_ended придёт от ядра при восстановлении связи
    console.warn("rchat: logout", error);
  }
  setState(
    "accounts",
    profile,
    produce((s) => {
      s.session = null;
      s.activeChatId = null;
      s.chats = [];
      s.messages = {};
      s.receipts = {};
      s.members = {};
      s.unread = {};
      s.presence = {};
      s.replyTo = null;
      s.sendError = null;
      s.authError = null;
    })
  );
}

/** Подключённые аккаунты (профили): подписки на события ставятся один раз. */
const connected = new Set<string>();

/** Подключает соединение аккаунта (профиля) к шлюзу. */
export function connectProfile(profile: string): void {
  const connection = apiFor(profile);
  if (!connected.has(profile)) {
    connected.add(profile);
    connection.onConnect(() => {
      setState("linkUp", true);
      void boot(profile);
      void loadProfiles();
    });
    connection.onDisconnect(() => setState("linkUp", false));
    connection.onEvent((event) => applyEvent(profile, event));
  }
  connection.connect();
}

/** Закрывает соединение аккаунта (профиля). */
export function disconnectProfile(profile: string): void {
  closeProfile(profile);
  connected.delete(profile);
}

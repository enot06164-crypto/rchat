/**
 * Реактивное хранилище SolidJS (createStore) и все действия приложения.
 *
 * Источник истины — локальное хранилище ядра: список чатов берётся из
 * `get_chats`, история — `get_messages`, receipts — `get_receipts`.  События
 * (`§7 docs/ffi.md`) обновляют копии данных мгновенно; после `event_lagged`
 * хранилище перечитывается с ядра.
 */

import { createStore } from "solid-js/store";
import {
  api,
  chatApi,
  describeError,
  listProfiles,
} from "./api";
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
} from "../types/rchat";

export interface AppState {
  /** Связь браузера с шлюзом (UI-уровень, к состояниям FFI отношения не имеет). */
  linkUp: boolean;
  /** Состояние соединения ядра с сервером (§5.1). */
  connection: ConnectionState;
  session: Session | null;
  /** Присутствие из события `presence_changed` (user_id → online). */
  presence: Record<string, boolean>;
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
  authError: string | null;
  sendError: string | null;
  newChatError: string | null;
  /** Режим чатов по умолчанию: облачный (`cloud`) или приватный (`private`). */
  defaultChatMode: ChatPrivacy;
  /** Найденные сообщения (поиск по истории; только для облачного режима). */
  searchHits: SearchHit[];
  /** Идёт ли поиск по сообщениям. */
  searchBusy: boolean;
  /** Профиль (аккаунт) шлюза, с которым соединён клиент. */
  profile: string;
  /** Профили, известные шлюзу. */
  profiles: string[];
}

const INITIAL_STATE: AppState = {
  linkUp: false,
  connection: "disconnected",
  session: null,
  presence: {},
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
  newChatError: null,
  defaultChatMode: "cloud",
  searchHits: [],
  searchBusy: false,
  profile: "default",
  profiles: ["default"],
};

export const [state, setState] = createStore<AppState>(INITIAL_STATE);

// ---------------------------------------------------------------------------
// Вспомогательные функции

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

function upsertChat(chat: Chat): void {
  setState("chats", (prev) => {
    const rest = prev.filter((c) => c.chat_id !== chat.chat_id);
    return [...rest, chat].sort(byCreatedDesc);
  });
}

function mergeMessages(chatId: string, fetched: Message[]): void {
  setState("messages", chatId, (prev = []) => {
    const map = new Map<string, Message>();
    for (const message of fetched) map.set(message.local_id, message);
    for (const message of prev)
      if (!map.has(message.local_id)) map.set(message.local_id, message);
    return sortMessages([...map.values()]);
  });
}

function mergeSingleMessage(message: Message): void {
  const existing = state.messages[message.chat_id] ?? [];
  if (
    message.body === null &&
    message.server_id !== null &&
    existing.some((m) => m.server_id === message.server_id)
  ) {
    // Ядро зеркалит отправителю его сообщение (для других устройств)
    // с неизвестным local_id ("s:<server_id>") и body null; у нас уже есть
    // полная копия этого сообщения — не создаём пустой пузырь.
    return;
  }
  setState("messages", message.chat_id, (prev = []) =>
    upsertSorted(prev, message)
  );
  const known = state.chats.some((c) => c.chat_id === message.chat_id);
  if (!known) void refreshChats();
}

function setOutboxState(localId: string, status: MessageStatus, error: string | null): void {
  for (const chatId of Object.keys(state.messages)) {
    const list = state.messages[chatId] ?? [];
    if (!list.some((m) => m.local_id === localId)) continue;
    setState(
      "messages",
      chatId,
      (prev = []) => prev.map((m) => (m.local_id === localId ? { ...m, status } : m))
    );
    if (status === "failed" && chatId === state.activeChatId) {
      setState("sendError", error ?? "не удалось отправить сообщение");
    }
    return;
  }
}

/** Максимальный `read_up_to` по всем собеседникам чата («прочитано»). */
export function readUpToFor(chatId: string): number {
  const receipts = state.receipts[chatId] ?? [];
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

let booting = false;

/**
 * Читает состояние ядра.  Вызывается после каждого успешного подключения
 * WebSocket (см. App.tsx); локальные данные доступны и до установки
 * соединения с сервером.
 */
export async function boot(): Promise<void> {
  if (booting) return;
  booting = true;
  try {
    const connection = await chatApi.getConnectionState();
    setState("connection", connection.state as ConnectionState);
    const session = (await chatApi.getSession()) as Session | null;
    setState("session", session);
    await loadSettings();
    if (session !== null) {
      const chats = (await chatApi.getChats()) as Chat[];
      setState("chats", chats.sort(byCreatedDesc));
      // Deep-link для смоков и закладок: /?chat=<chat_id> открывает чат сразу
      const want = new URLSearchParams(window.location.search).get("chat");
      if (want !== null && chats.some((c) => c.chat_id === want)) {
        void openChat(want);
      }
    }
    setState("booted", true);
  } catch {
    // шлюз ещё не готов — App.tsx повторит boot после реконнекта
    setState("booted", false);
  } finally {
    booting = false;
  }
}

/** Ключ профиля в localStorage (помним между перезагрузками). */
const PROFILE_STORAGE_KEY = "rchat.profile";

/** Текущий профиль: из localStorage, иначе `default`. */
export function storedProfile(): string {
  try {
    const value = window.localStorage.getItem(PROFILE_STORAGE_KEY);
    return value !== null && value.trim() !== "" ? value.trim() : "default";
  } catch {
    return "default";
  }
}

/** Запоминает выбранный профиль. */
function storeProfile(profile: string): void {
  try {
    window.localStorage.setItem(PROFILE_STORAGE_KEY, profile);
  } catch {
    /* localStorage может быть недоступен — не критично */
  }
}

/** Читает список профилей шлюза (для выбора аккаунта). */
export async function loadProfiles(): Promise<void> {
  const profiles = await listProfiles();
  if (profiles.length > 0) {
    setState("profiles", profiles);
  }
}

/**
 * Переключает профиль (аккаунт): соединение переоткрывается, состояние
 * очищается — у каждого профиля свои чаты и сообщения.
 */
export async function switchProfile(profile: string): Promise<void> {
  const name = profile.trim();
  if (name === "" || name === state.profile) return;
  storeProfile(name);
  setState({
    profile: name,
    session: null,
    chats: [],
    messages: {},
    receipts: {},
    members: {},
    activeChatId: null,
    unread: {},
    replyTo: null,
    booted: false,
    authError: null,
  });
  api.setProfile(name);
  await loadProfiles();
  await boot();
}

/**
 * Настройки клиента: режим чатов по умолчанию (облачный или приватный).
 */
export async function loadSettings(): Promise<void> {
  try {
    const settings = await chatApi.getSettings();
    setState("defaultChatMode", settings.default_chat_privacy === "private" ? "private" : "cloud");
  } catch {
    /* шлюз не готов — останется текущее значение */
  }
}

/** Меняет режим чатов по умолчанию (применяется к новым чатам). */
export async function setDefaultChatMode(mode: ChatPrivacy): Promise<void> {
  setState("defaultChatMode", mode);
  try {
    await chatApi.setDefaultChatMode(mode);
  } catch (error) {
    setState("defaultChatMode", mode === "cloud" ? "private" : "cloud");
    console.warn("rchat: set default chat mode", error);
  }
}

/**
 * Поиск по сообщениям аккаунта.  Работает в облачном режиме: сервер
 * расшифровывает содержимое ключом чата и ищет по нему.  В приватных чатах
 * содержимое серверу недоступно, поэтому в выдаче их нет.
 */
export async function searchMessages(query: string): Promise<void> {
  const trimmed = query.trim();
  if (trimmed === "") {
    setState({ searchHits: [], searchBusy: false });
    return;
  }
  setState("searchBusy", true);
  try {
    const result = await chatApi.searchMessages(trimmed);
    setState("searchHits", result.hits ?? []);
  } catch (error) {
    console.warn("rchat: search messages", error);
    setState("searchHits", []);
  } finally {
    setState("searchBusy", false);
  }
}

export async function refreshChats(): Promise<void> {
  try {
    const chats = (await chatApi.getChats()) as Chat[];
    setState("chats", chats.sort(byCreatedDesc));
  } catch {
    /* переподключение — состояние соединения придёт событием */
  }
}

/** Перечитывает состояние соединения ядра (после auth-переходов). */
async function updateConnection(): Promise<void> {
  try {
    const connection = await chatApi.getConnectionState();
    setState("connection", connection.state as ConnectionState);
  } catch {
    /* без соединения покажем прежнее состояние */
  }
}

/** Применяет одно событие ядра к хранилищу. */
export function applyEvent(event: RchatEvent): void {
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
          const session = (await chatApi.getSession()) as Session | null;
          setState("session", session);
          if (session !== null) void refreshChats();
        } catch {
          /* проигнорируем: следующий event повторит попытку */
        }
      })();
      return;
    }
    case "session_ended": {
      setState({
        session: null,
        activeChatId: null,
        messages: {},
        receipts: {},
        members: {},
        unread: {},
        presence: {},
        replyTo: null,
        sendError: null,
      });
      return;
    }
    case "chat_upserted": {
      upsertChat(event.data as Chat);
      return;
    }
    case "message_received": {
      const message = event.data as Message;
      mergeSingleMessage(message);
      const meId = state.session?.user_id ?? null;
      if (meId !== null && message.sender_user_id !== meId) {
        if (message.chat_id === state.activeChatId) {
          // чат открыт — сообщение прочитано сразу
          if (message.server_id !== null) {
            void chatApi
              .markRead(message.chat_id)
              .catch(() => undefined);
          }
        } else {
          setState("unread", message.chat_id, (prev = 0) => prev + 1);
        }
      }
      return;
    }
    case "message_updated": {
      mergeSingleMessage(event.data as Message);
      return;
    }
    case "outbox_changed": {
      const data = event.data as {
        local_id: string;
        state: Exclude<MessageStatus, "none">;
        error: string | null;
      };
      setOutboxState(data.local_id, data.state, data.error);
      return;
    }
    case "receipt_updated": {
      const receipt = event.data as Receipt;
      setState("receipts", receipt.chat_id, (prev = []) => {
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
      void refreshChats();
      const active = state.activeChatId;
      if (active !== null) void loadChatHistory(active);
      return;
    }
    case "presence_changed": {
      // статус собеседника для подзаголовка шапки («в сети» / «был(а) недавно»);
      // до первого события показываем только признак шифрования
      const data = event.data as { user_id: string; online: boolean };
      setState("presence", data.user_id, data.online);
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

export async function openChat(chatId: string): Promise<void> {
  setState({ activeChatId: chatId, sendError: null, replyTo: null });
  setState("unread", chatId, 0);
  void loadChatHistory(chatId);
}

/** Начать ответ на сообщение (реплай в композере, FFI `reply_to_server_id`). */
export function setReplyTo(message: Message): void {
  setState("replyTo", message);
}

export function clearReply(): void {
  setState("replyTo", null);
}

/**
 * Удаление сообщения (FFI `delete_message`). Локально помечаем как удалённое
 * сразу; подтверждение придёт событием `message_updated` идемпотентно.
 */
export async function deleteMessage(chatId: string, serverId: number): Promise<void> {
  try {
    await chatApi.deleteMessage(chatId, serverId);
    setState("messages", chatId, (prev = []) =>
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
export async function ensureMembers(chatId: string): Promise<void> {
  if (state.members[chatId] !== undefined) return;
  try {
    const members = (await chatApi.getChatMembers(chatId)) as ChatMember[];
    setState(
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
export async function loadCommonGroups(peerUserId: string): Promise<Chat[]> {
  const groups = state.chats.filter((chat) => chat.kind === "group");
  const common: Chat[] = [];
  for (const chat of groups) {
    await ensureMembers(chat.chat_id);
    const members = state.members[chat.chat_id] ?? {};
    if (peerUserId in members) common.push(chat);
  }
  return common;
}

async function loadChatHistory(chatId: string): Promise<void> {
  const job = (async () => {
    const chat = state.chats.find((c) => c.chat_id === chatId);
    const requests: Promise<void>[] = [];

    requests.push(
      (async () => {
        const fresh = (await chatApi.getMessages(chatId)) as Message[];
        mergeMessages(chatId, fresh.slice().reverse()); // ядро: новые первыми
      })()
    );

    requests.push(
      (async () => {
        const receipts = (await chatApi.getReceipts(chatId)) as Receipt[];
        setState("receipts", chatId, receipts);
      })()
    );

    if (chat !== undefined && chat.kind === "group") {
      requests.push(
        (async () => {
          const members = (await chatApi.getChatMembers(chatId)) as ChatMember[];
          setState(
            "members",
            chatId,
            Object.fromEntries(members.map((m) => [m.user_id, m]))
          );
        })()
      );
    }

    const results = await Promise.allSettled(requests);
    for (const result of results) {
      if (result.status === "rejected") console.warn(result.reason);
    }
    try {
      await chatApi.markRead(chatId);
    } catch {
      /* отметка о прочтении не критична для UI */
    }
  })();
  await job;
}

export async function sendCurrentText(
  text: string,
  attachments: AttachmentUpload[] = []
): Promise<void> {
  const chatId = state.activeChatId;
  const session = state.session;
  const trimmed = text.trim();
  if (chatId === null || session === null) return;
  if (trimmed === "" && attachments.length === 0) return;

  setState("sendError", null);
  const replyTo = state.replyTo;
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
  setState("messages", chatId, (prev = []) =>
    optimisticMessages.reduce((acc, message) => upsertSorted(acc, message), prev)
  );

  try {
    const result = (await chatApi.sendMessage(
      chatId,
      trimmed,
      replyTo !== null ? replyTo.server_id : null,
      attachments
    )) as { local_id?: string; local_ids?: string[] };
    setState(
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
    clearReply();
  } catch (error) {
    setState(
      "messages",
      chatId,
      (prev = []) =>
        prev.map((m) =>
          optimisticMessages.some((item) => item.local_id === m.local_id)
            ? { ...m, status: "failed" as MessageStatus }
            : m
        )
    );
    setState("sendError", describeError(error));
  }
}

export async function createDirectChat(
  usernameRaw: string,
  mode?: ChatPrivacy
): Promise<Chat | null> {
  const username = usernameRaw.trim().replace(/^@+/, "");
  setState("newChatError", null);
  if (username === "") {
    setState("newChatError", "введите username собеседника");
    return null;
  }
  try {
    const chat = (await chatApi.createDirectChat(username, mode)) as Chat;
    upsertChat(chat);
    return chat;
  } catch (error) {
    setState("newChatError", describeError(error));
    return null;
  }
}

export async function login(username: string, password: string): Promise<boolean> {
  setState({ authError: null, busy: true });
  try {
    const session = (await chatApi.login(username, password)) as Session;
    setState("session", session);
    void refreshChats();
    void updateConnection();
    return true;
  } catch (error) {
    setState("authError", describeError(error));
    return false;
  } finally {
    setState("busy", false);
  }
}

export async function register(
  username: string,
  displayName: string,
  password: string
): Promise<boolean> {
  setState({ authError: null, busy: true });
  try {
    const session = (await chatApi.register(username, displayName, password)) as Session;
    setState("session", session);
    void refreshChats();
    void updateConnection();
    return true;
  } catch (error) {
    setState("authError", describeError(error));
    return false;
  } finally {
    setState("busy", false);
  }
}

export async function logout(): Promise<void> {
  try {
    await chatApi.logout();
  } catch (error) {
    // даже при ошибке (например not_connected) сеанс сбрасываем локально —
    // событие session_ended придёт от ядра при восстановлении связи
    console.warn("rchat: logout", error);
  }
  setState({
    session: null,
    activeChatId: null,
    chats: [],
    messages: {},
    receipts: {},
    members: {},
    unread: {},
    presence: {},
    replyTo: null,
    sendError: null,
    authError: null,
  });
}

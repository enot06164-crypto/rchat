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
  chatApi,
  describeError,
} from "./api";
import type {
  Chat,
  ChatMember,
  ConnectionState,
  Message,
  MessageStatus,
  RchatEvent,
  Receipt,
  Session,
} from "../types/rchat";

export interface AppState {
  /** Связь браузера с шлюзом (UI-уровень, к состояниям FFI отношения не имеет). */
  linkUp: boolean;
  /** Состояние соединения ядра с сервером (§5.1). */
  connection: ConnectionState;
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
  booted: boolean;
  busy: boolean;
  authError: string | null;
  sendError: string | null;
  newChatError: string | null;
}

const INITIAL_STATE: AppState = {
  linkUp: false,
  connection: "disconnected",
  session: null,
  chats: [],
  messages: {},
  receipts: {},
  members: {},
  activeChatId: null,
  unread: {},
  booted: false,
  busy: false,
  authError: null,
  sendError: null,
  newChatError: null,
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
    case "warning": {
      const data = event.data as { message: string };
      console.warn("rchat warning:", data.message);
      return;
    }
    default:
      // presence_changed, sync_progress, resync_performed и call_* событий
      // v1 веб-UI не отображает: расширенная логика появится на этапе звонков.
      return;
  }
}

// ---------------------------------------------------------------------------
// Действия пользователя

export async function openChat(chatId: string): Promise<void> {
  setState({ activeChatId: chatId, sendError: null });
  setState("unread", chatId, 0);
  void loadChatHistory(chatId);
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

export async function sendCurrentText(text: string): Promise<void> {
  const chatId = state.activeChatId;
  const session = state.session;
  const trimmed = text.trim();
  if (chatId === null || session === null || trimmed === "") return;

  setState("sendError", null);
  const optimistic: Message = {
    local_id: `web-${Date.now()}-${Math.random().toString(16).slice(2)}`,
    server_id: null,
    chat_id: chatId,
    sender_user_id: session.user_id,
    sender_device_id: "",
    content_kind: "text",
    body: trimmed,
    reply_to_server_id: null,
    status: "pending",
    created_at_ms: Date.now(),
    edited_at_ms: null,
    deleted_at_ms: null,
  };
  setState("messages", chatId, (prev = []) => upsertSorted(prev, optimistic));

  try {
    const result = (await chatApi.sendMessage(chatId, trimmed)) as {
      local_id: string;
    };
    setState(
      "messages",
      chatId,
      (prev = []) =>
        prev.map((m) =>
          m.local_id === optimistic.local_id
            ? { ...m, local_id: result.local_id, status: "sending" as MessageStatus }
            : m
        )
    );
  } catch (error) {
    setState(
      "messages",
      chatId,
      (prev = []) =>
        prev.map((m) =>
          m.local_id === optimistic.local_id
            ? { ...m, status: "failed" as MessageStatus }
            : m
        )
    );
    setState("sendError", describeError(error));
  }
}

export async function createDirectChat(usernameRaw: string): Promise<Chat | null> {
  const username = usernameRaw.trim().replace(/^@+/, "");
  setState("newChatError", null);
  if (username === "") {
    setState("newChatError", "введите username собеседника");
    return null;
  }
  try {
    const chat = (await chatApi.createDirectChat(username)) as Chat;
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
    sendError: null,
    authError: null,
  });
}

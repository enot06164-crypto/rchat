/**
 * WebSocket-клиент шлюза (`rchat.gateway`, этап H).
 *
 * Протокол совпадает с FFI (`docs/ffi.md` §4): браузер шлёт JSON с `@type`,
 * шлюз возвращает `{@type:"ok"|"error","request":…,"data":…}` в порядке
 * отправки запросов, а события ядра приходят как
 * `{@type:"event","event":…,"data":…}` всем открытым сокетам.
 *
 * Так как шлюз обрабатывает кадры одного сокета строго последовательно,
 * ответ можно сопоставлять запросу по FIFO-очереди — надёжно и без
 * расширения протокола идентификаторами.
 */

import type {
  AttachmentUpload,
  ErrorEnvelope,
  OkEnvelope,
  RchatEvent,
  SearchHit,
  UserSummary,
} from "../types/rchat";

export class RchatApiError extends Error {
  readonly code: string;
  readonly retryable: boolean;
  readonly serverCode?: number;
  readonly serverCodeName?: string;

  constructor(
    code: string,
    message: string,
    retryable: boolean,
    serverCode?: number,
    serverCodeName?: string
  ) {
    super(message);
    this.name = "RchatApiError";
    this.code = code;
    this.retryable = retryable;
    if (serverCode !== undefined) this.serverCode = serverCode;
    if (serverCodeName !== undefined) this.serverCodeName = serverCodeName;
  }
}

interface PendingRequest {
  resolve: (data: unknown) => void;
  reject: (error: RchatApiError) => void;
  method: string;
}

export type EventListener = (event: RchatEvent) => void;
export type ConnectListener = () => void;
export type DisconnectListener = () => void;

const INITIAL_BACKOFF_MS = 800;
const MAX_BACKOFF_MS = 5000;

/**
 * Токен устройства браузера (мультипользовательский режим шлюза,
 * `--multiuser`): 32 случайных байта, hex.  Генерируется один раз и живёт в
 * `localStorage`, как сессия в Telegram Web — после перезагрузки страницы и
 * повторного захода по ссылке аккаунт сохраняется.  Шлюз выводит из токена
 * профиль устройства, поэтому произвольный `?profile=` не нужен: каждый
 * браузер получает собственное изолированное ядро со входом/регистрацией.
 */
const DEVICE_STORAGE_KEY = "rchat.device";

function deviceToken(): string {
  try {
    const existing = window.localStorage.getItem(DEVICE_STORAGE_KEY);
    if (existing !== null && /^[0-9a-f]{64}$/.test(existing)) return existing;
    const bytes = new Uint8Array(32);
    crypto.getRandomValues(bytes);
    const token = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
    window.localStorage.setItem(DEVICE_STORAGE_KEY, token);
    return token;
  } catch {
    // localStorage недоступен — временный токен на жизнь страницы.
    const bytes = new Uint8Array(32);
    crypto.getRandomValues(bytes);
    return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
  }
}

/** Включён ли мультипользовательский режим (адрес страницы `?multiuser=1`). */
export function isMultiuser(): boolean {
  return new URLSearchParams(window.location.search).get("multiuser") === "1";
}

export class GatewayApi {
  /** Профиль (аккаунт) шлюза: выбирается при подключении (`?profile=`). */
  private profile = "default";
  private ws: WebSocket | null = null;
  private pending: PendingRequest[] = [];
  private eventListeners: EventListener[] = [];
  private connectListeners: ConnectListener[] = [];
  private disconnectListeners: DisconnectListener[] = [];
  private backoffMs = INITIAL_BACKOFF_MS;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private stopped = false;
  /** Было ли подключение (до первого connect() профиль просто запоминаем). */
  private everConnected = false;

  /** Подключается к шлюзу и переустанавливает связь после обрывов (backoff). */
  connect(): void {
    this.stopped = false;
    this.everConnected = true;
    this.attemptConnect();
  }

  /** Текущий профиль (аккаунт) шлюза. */
  getProfile(): string {
    return this.profile;
  }

  /**
   * Меняет профиль и переподключается: у каждого профиля свои БД, секреты и
   * E2E-состояние, поэтому соединение нужно переоткрыть.
   */
  setProfile(profile: string): void {
    if (this.profile === profile) return;
    this.profile = profile;
    // Если подключения ещё не было — профиль просто запомнится до connect().
    if (!this.everConnected) return;
    this.stopped = true;
    this.failPending(
      new RchatApiError("transport", "профиль сменён, переподключаемся", true)
    );
    this.ws?.close();
    this.ws = null;
    this.stopped = false;
    this.backoffMs = INITIAL_BACKOFF_MS;
    this.attemptConnect();
  }

  stop(): void {
    this.stopped = true;
    if (this.retryTimer !== null) {
      clearTimeout(this.retryTimer);
      this.retryTimer = null;
    }
    this.failPending(new RchatApiError("transport", "соединение закрыто", true));
    this.ws?.close();
    this.ws = null;
  }

  onEvent(listener: EventListener): () => void {
    this.eventListeners.push(listener);
    return () => {
      this.eventListeners = this.eventListeners.filter((x) => x !== listener);
    };
  }

  /** Вызывается после первого успешного подключения и после каждого обрыва. */
  onConnect(listener: ConnectListener): () => void {
    this.connectListeners.push(listener);
    return () => {
      this.connectListeners = this.connectListeners.filter(
        (x) => x !== listener
      );
    };
  }

  onDisconnect(listener: DisconnectListener): () => void {
    this.disconnectListeners.push(listener);
    return () => {
      this.disconnectListeners = this.disconnectListeners.filter(
        (x) => x !== listener
      );
    };
  }

  /**
   * Один RPC-вызов.  Запросы могут отправляться параллельно: шлюз
   * отвечает в порядке поступления, поэтому FIFO-сопоставление корректно.
   */
  call<T>(method: string, params: Record<string, unknown> = {}): Promise<T> {
    const ws = this.ws;
    if (ws === null || ws.readyState !== WebSocket.OPEN) {
      return Promise.reject(
        new RchatApiError("transport", "шлюз не подключён", true)
      );
    }
    return new Promise<T>((resolve, reject) => {
      this.pending.push({
        resolve: (data) => resolve(data as T),
        reject,
        method,
      });
      ws.send(JSON.stringify({ "@type": method, ...params }));
    });
  }

  // -------------------------------------------------------------------------

  /**
   * Причина последнего отказа шлюза (например, мультипользовательский режим
   * требует токен устройства).  Показывается на экране ожидания, чтобы не
   * бесконечно крутился спиннер.
   */
  private connectError: string | null = null;

  /** Причина последнего отказа шлюза (или `null`). */
  get lastConnectError(): string | null {
    return this.connectError;
  }

  private attemptConnect(): void {
    const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
    const query = new URLSearchParams({ profile: this.profile });
    if (isMultiuser()) {
      // Мультипользовательский режим шлюза: профиль на его стороне
      // выводится из токена устройства, `profile` игнорируется.
      query.set("device", deviceToken());
    }
    const url = `${proto}//${window.location.host}/ws?${query}`;
    const ws = new WebSocket(url);

    ws.onopen = () => {
      if (this.stopped) {
        ws.close();
        return;
      }
      this.ws = ws;
      this.connectError = null;
      this.backoffMs = INITIAL_BACKOFF_MS;
      for (const listener of [...this.connectListeners]) listener();
    };

    ws.onmessage = (message) => {
      if (typeof message.data === "string") this.handleMessage(message.data);
    };

    ws.onclose = () => {
      const wasOpen = this.ws === ws;
      if (this.ws === ws) this.ws = null;
      if (wasOpen) {
        this.failPending(
          new RchatApiError("transport", "соединение с шлюзом закрыто", true)
        );
      }
      // О разрыве сообщаем и при отказе шлюза (соединение не открылось): так
      // интерфейс покажет причину, а не бесконечный спиннер.
      if (wasOpen || this.connectError !== null) {
        for (const listener of [...this.disconnectListeners]) listener();
      }
      this.scheduleReconnect();
    };

    ws.onerror = () => {
      /* последует событие close; соединение будет повторено с backoff */
    };
  }

  private scheduleReconnect(): void {
    if (this.stopped || this.retryTimer !== null) return;
    const delay = this.backoffMs;
    this.backoffMs = Math.min(MAX_BACKOFF_MS, this.backoffMs * 2);
    this.retryTimer = setTimeout(() => {
      this.retryTimer = null;
      if (!this.stopped) this.attemptConnect();
    }, delay);
  }

  private handleMessage(raw: string): void {
    let parsed: { "@type"?: unknown; request?: unknown } | null = null;
    try {
      parsed = JSON.parse(raw) as { "@type"?: unknown; request?: unknown };
    } catch (error) {
      console.warn("rchat: кадр не разобран как JSON", error);
      return;
    }
    if (parsed === null || typeof parsed !== "object") return;

    const kind =
      typeof parsed["@type"] === "string" ? parsed["@type"] : undefined;

    if (kind === "event") {
      const event = parsed as unknown as RchatEvent;
      for (const listener of [...this.eventListeners]) listener(event);
      return;
    }

    if (kind === "ok" || kind === "error") {
      if (kind === "error" && typeof parsed["request"] !== "string") {
        // Отказ шлюза в самом соединении (например, мультипользовательский
        // режим требует токен устройства): в конверте нет имени запроса.
        // Запоминаем причину — интерфейс покажет её вместо бесконечного
        // ожидания — и обрываем запросы, которые уже висели.
        const error = parsed as ErrorEnvelope;
        this.connectError = error.message;
        this.failPending(
          new RchatApiError(error.code, error.message, false)
        );
        console.warn("rchat: шлюз отказал в соединении", error.message);
        return;
      }
      const pending = this.pending.shift();
      if (pending === undefined) {
        console.warn("rchat: ответ без ожидающего запроса", raw);
        return;
      }
      if (kind === "ok") {
        const ok = parsed as OkEnvelope;
        pending.resolve(ok.data);
      } else {
        const error = parsed as ErrorEnvelope;
        pending.reject(
          new RchatApiError(
            error.code,
            error.message,
            error.retryable,
            error.server_code,
            error.server_code_name
          )
        );
      }
      return;
    }

    console.warn("rchat: неизвестный конверт", raw);
  }

  private failPending(error: RchatApiError): void {
    const waiting = this.pending;
    this.pending = [];
    for (const pending of waiting) pending.reject(error);
  }
}

/**
 * Список профилей (аккаунтов) шлюза: короткое подключение без профиля
 * (`/ws?list=profiles`).  Ошибка не критична — показываем текущий.
 */
export async function listProfiles(): Promise<string[]> {
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  const query = new URLSearchParams({ list: "profiles" });
  if (isMultiuser()) query.set("device", deviceToken());
  const url = `${proto}//${window.location.host}/ws?${query}`;
  return new Promise<string[]>((resolve) => {
    let settled = false;
    const finish = (profiles: string[]): void => {
      if (settled) return;
      settled = true;
      resolve(profiles);
    };
    try {
      const ws = new WebSocket(url);
      const timer = setTimeout(() => {
        try {
          ws.close();
        } catch {
          /* соединение уже закрыто */
        }
        finish([]);
      }, 5000);
      ws.onmessage = (message) => {
        if (typeof message.data !== "string") return;
        try {
          const parsed = JSON.parse(message.data) as {
            "@type"?: string;
            data?: { profiles?: string[] };
          };
          if (parsed["@type"] === "ok") {
            clearTimeout(timer);
            ws.close();
            finish(parsed.data?.profiles ?? []);
          }
        } catch {
          /* не наш ответ */
        }
      };
      ws.onerror = () => {
        clearTimeout(timer);
        finish([]);
      };
      ws.onclose = () => {
        clearTimeout(timer);
        finish([]);
      };
    } catch {
      finish([]);
    }
  });
}

// ---------------------------------------------------------------------------
// Соединения: по одному на аккаунт (профиль)
// ---------------------------------------------------------------------------

/** Соединения по профилям: у каждого аккаунта свой WebSocket и свой core. */
const apis = new Map<string, GatewayApi>();

/** Соединение аккаунта (профиля); создаётся при первом обращении. */
export function apiFor(profile: string): GatewayApi {
  let connection = apis.get(profile);
  if (connection === undefined) {
    connection = new GatewayApi();
    connection.setProfile(profile);
    apis.set(profile, connection);
  }
  return connection;
}

/** Закрывает соединение аккаунта (профиля). */
export function closeProfile(profile: string): void {
  const connection = apis.get(profile);
  if (connection === undefined) return;
  connection.stop();
  apis.delete(profile);
}

// ---------------------------------------------------------------------------
// Типизированные тонкие обёртки над командами FFI (§5)

/** Команды FFI для конкретного аккаунта (профиля). */
export function chatApiFor(profile: string) {
  const connection = apiFor(profile);
  return {
  getConnectionState: () =>
    connection.call<{ state: string }>("get_connection_state"),
  getSession: () => connection.call<unknown>("get_session"),
  getChats: () => connection.call<unknown[]>("get_chats"),
  getChatMembers: (chatId: string) =>
    connection.call<unknown[]>("get_chat_members", { chat_id: chatId }),
  /** Запрос собеседнику перевести чат в приватный режим (E2E). */
  requestPrivateMode: (chatId: string) =>
    connection.call<{ pending: boolean }>("request_private_mode", { chat_id: chatId }),
  /** Запрос-сообщение в чат: карточка «Запрос на приватный режим». */
  sendPrivateModeRequest: (chatId: string, enable = true) =>
    connection.call<{ local_id: string }>("send_private_mode_request", {
      chat_id: chatId,
      enable,
    }),
  /** Отзыв собственного запроса на приватный режим. */
  cancelPrivateMode: (chatId: string) =>
    connection.call<unknown>("cancel_private_mode", { chat_id: chatId }),
  /** Ответ на запрос: принять или отклонить. */
  resolvePrivateMode: (chatId: string, accept: boolean) =>
    connection.call<{ switched: boolean }>("resolve_private_mode", {
      chat_id: chatId,
      accept,
    }),
  /** Поиск пользователей по префиксу username (для выбора собеседника). */
  searchUsers: (query: string, limit = 20) =>
    connection.call<{ users: UserSummary[] }>("search_users", { query, limit }),
  getMessages: (chatId: string, limit = 100) =>
    connection.call<unknown[]>("get_messages", { chat_id: chatId, limit }),
  /** Страница истории перед `beforeServerId` (с сервера). */
  fetchHistory: (chatId: string, beforeServerId: number, limit = 100) =>
    connection.call<unknown[]>("fetch_history", {
      chat_id: chatId,
      before_server_id: beforeServerId,
      limit,
    }),
  getReceipts: (chatId: string) =>
    connection.call<unknown[]>("get_receipts", { chat_id: chatId }),
  createDirectChat: (username: string, mode?: string) =>
    connection.call<unknown>("create_direct_chat", { peer_username: username, mode }),
  /** Настройки клиента: режим чатов по умолчанию. */
  getSettings: () =>
    connection.call<{ default_chat_mode: string; default_chat_privacy: string }>(
      "get_settings"
    ),
  /** Сменить режим чатов по умолчанию: `cloud` или `private`. */
  setDefaultChatMode: (mode: string) =>
    connection.call<{ default_chat_mode: string }>("set_default_chat_mode", { mode }),
  /** Поиск по сообщениям аккаунта (работает в облачном режиме). */
  searchMessages: (query: string, limit = 50) =>
    connection.call<{ hits: SearchHit[] }>("search_messages", { query, limit }),
  sendMessage: (
    chatId: string,
    text: string,
    replyToServerId: number | null = null,
    attachments: AttachmentUpload[] = []
  ) => {
    const payload: Record<string, unknown> = { chat_id: chatId, text };
    if (replyToServerId !== null) payload.reply_to_server_id = replyToServerId;
    if (attachments.length === 1) {
      payload.attachment = attachments[0];
    } else if (attachments.length > 1) {
      payload.attachments = attachments;
    }
    return connection.call<unknown>("send_message", payload) as Promise<{
      local_id: string;
    }>;
  },
  markRead: (chatId: string) =>
    connection.call<unknown>("mark_read", { chat_id: chatId }),
  /** Возвращает упавшее сообщение в очередь (та же запись, без новой копии). */
  retryMessage: (localId: string) =>
    connection.call<{ requeued: boolean }>("retry_message", { local_id: localId }),
  downloadAttachment: (serverId: number) =>
    connection.call<{
      kind: string;
      name: string;
      mime: string;
      size: number;
      data_base64: string;
      thumb_base64: string | null;
    }>("download_attachment", { server_id: serverId }),
  deleteMessage: (chatId: string, serverId: number) =>
    connection.call<unknown>("delete_message", { chat_id: chatId, server_id: serverId }),
  login: (username: string, password: string) =>
    connection.call<unknown>("login", { username, password }),
  register: (username: string, displayName: string, password: string) => {
    const params: Record<string, unknown> = { username, password };
    if (displayName !== "") params.display_name = displayName;
    return connection.call<unknown>("register", params);
  },
  logout: () => connection.call<unknown>("logout"),
  };
}

/** Человекочитаемое описание ошибки для UI. */
export function describeError(error: unknown): string {
  if (error instanceof RchatApiError) {
    return error.message || error.code;
  }
  if (error instanceof Error) return error.message;
  return String(error);
}

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

export class GatewayApi {
  private ws: WebSocket | null = null;
  private pending: PendingRequest[] = [];
  private eventListeners: EventListener[] = [];
  private connectListeners: ConnectListener[] = [];
  private disconnectListeners: DisconnectListener[] = [];
  private backoffMs = INITIAL_BACKOFF_MS;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private stopped = false;

  /** Подключается к шлюзу и переустанавливает связь после обрывов (backoff). */
  connect(): void {
    this.stopped = false;
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

  private attemptConnect(): void {
    const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
    const url = `${proto}//${window.location.host}/ws`;
    const ws = new WebSocket(url);

    ws.onopen = () => {
      if (this.stopped) {
        ws.close();
        return;
      }
      this.ws = ws;
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
    let parsed: { "@type"?: unknown } | null = null;
    try {
      parsed = JSON.parse(raw) as { "@type"?: unknown };
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

export const api = new GatewayApi();

// ---------------------------------------------------------------------------
// Типизированные тонкие обёртки над командами FFI (§5)

export const chatApi = {
  getConnectionState: () =>
    api.call<{ state: string }>("get_connection_state"),
  getSession: () => api.call<unknown>("get_session"),
  getChats: () => api.call<unknown[]>("get_chats"),
  getChatMembers: (chatId: string) =>
    api.call<unknown[]>("get_chat_members", { chat_id: chatId }),
  getMessages: (chatId: string, limit = 100) =>
    api.call<unknown[]>("get_messages", { chat_id: chatId, limit }),
  getReceipts: (chatId: string) =>
    api.call<unknown[]>("get_receipts", { chat_id: chatId }),
  createDirectChat: (username: string, mode?: string) =>
    api.call<unknown>("create_direct_chat", { peer_username: username, mode }),
  /** Настройки клиента: режим чатов по умолчанию. */
  getSettings: () =>
    api.call<{ default_chat_mode: string; default_chat_privacy: string }>(
      "get_settings"
    ),
  /** Сменить режим чатов по умолчанию: `cloud` или `private`. */
  setDefaultChatMode: (mode: string) =>
    api.call<{ default_chat_mode: string }>("set_default_chat_mode", { mode }),
  /** Поиск по сообщениям аккаунта (работает в облачном режиме). */
  searchMessages: (query: string, limit = 50) =>
    api.call<{ hits: SearchHit[] }>("search_messages", { query, limit }),
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
    return api.call<unknown>("send_message", payload) as Promise<{
      local_id: string;
    }>;
  },
  markRead: (chatId: string) =>
    api.call<unknown>("mark_read", { chat_id: chatId }),
  downloadAttachment: (serverId: number) =>
    api.call<{
      kind: string;
      name: string;
      mime: string;
      size: number;
      data_base64: string;
      thumb_base64: string | null;
    }>("download_attachment", { server_id: serverId }),
  deleteMessage: (chatId: string, serverId: number) =>
    api.call<unknown>("delete_message", { chat_id: chatId, server_id: serverId }),
  login: (username: string, password: string) =>
    api.call<unknown>("login", { username, password }),
  register: (username: string, displayName: string, password: string) => {
    const params: Record<string, unknown> = { username, password };
    if (displayName !== "") params.display_name = displayName;
    return api.call<unknown>("register", params);
  },
  logout: () => api.call<unknown>("logout"),
};

/** Человекочитаемое описание ошибки для UI. */
export function describeError(error: unknown): string {
  if (error instanceof RchatApiError) {
    return error.message || error.code;
  }
  if (error instanceof Error) return error.message;
  return String(error);
}

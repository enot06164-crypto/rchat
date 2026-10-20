/**
 * Типы, зеркалирующие JSON Schema ядра (`schema/rchat.schema.json`) в объёме,
 * необходимом веб-клиенту.  Источник истины — FFI-документация `docs/ffi.md`
 * (§4 формат конверта, §5 объекты, §7 события).
 *
 * Конверт ответов шлюза (этап H, `rchat.gateway`) повторяет FFI:
 *   ok   → {"@type":"ok","request":<метод>,"data":<объект ответа>}
 *   error→ {"@type":"error","request":<метод>,"code","message","retryable"[,...]}
 *   event→ {"@type":"event","event":<имя>,"data":<payload>}
 */

// ---------------------------------------------------------------------------
// Сущности (§4, §5.6 — аватары)

export interface AvatarInfo {
  initials: string;
  color_index: number;
  color_hex: string;
}

export type ChatKind = "direct" | "group" | "unspecified";
export type E2eMode = "olm_v1" | "none_dev" | "unspecified" | "cloud";

export interface Chat {
  chat_id: string;
  kind: ChatKind;
  e2e_mode: E2eMode;
  /** Приватность чата: `cloud` — облачный (сервер знает ключ), `private` — E2E. */
  privacy: ChatPrivacy;
  title: string;
  /** Превью последнего сообщения — чтобы список чатов был заполнен сразу. */
  last_message: ChatPreview | null;
  peer_user_id: string | null;
  created_at_ms: number;
  updated_at_ms: number;
  last_server_id: number;
  avatar: AvatarInfo;
}

export type MemberRole = "owner" | "member" | "unspecified";

/** Краткая карточка пользователя — результат поиска по username. */
export interface UserSummary {
  user_id: string;
  username: string;
  display_name: string;
  avatar: AvatarInfo;
}

/** Превью последнего сообщения чата (для списка чатов). */
export interface ChatPreview {
  body: string | null;
  sender_user_id: string;
  content_kind: string;
  attachment_kind: string | null;
  created_at_ms: number;
}

/** Режим чатов по умолчанию: облачный или приватный (E2E). */
export type ChatPrivacy = "cloud" | "private";

/** Найденное сообщение (поиск по истории в облачном режиме). */
export interface SearchHit {
  chat_id: string;
  server_id: number;
  body: string;
  created_at_ms: number;
  chat_title: string;
}

export interface ChatMember {
  user_id: string;
  username: string;
  display_name: string;
  role: MemberRole;
  joined_at_ms: number;
  avatar: AvatarInfo;
}

export type MessageStatus = "pending" | "sending" | "sent" | "failed" | "none";

/** Вид вложения сообщения (как в FFI `content_kind` вложения). */
export type AttachmentKind = "photo" | "video" | "file" | "voice";

/** Метаданные вложения (содержимое — через `download_attachment`). */
export interface AttachmentInfo {
  kind: AttachmentKind;
  name: string;
  mime: string;
  size: number;
  /** Постер-кадр видео (отдельный блоб, как thumbnail в Telegram). */
  thumb?: AttachmentInfo;
  /** Пики звуковой волны (голосовое) — base64. */
  waveform?: string | null;
}

/** Вложение при отправке: содержимое в base64. */
export interface AttachmentUpload {
  kind: AttachmentKind;
  name: string;
  mime: string;
  size: number;
  data_base64: string;
  /** Постер-кадр видео (base64) — уходит отдельным блобом. */
  thumb_base64?: string | undefined;
  /** MIME-тип постера. */
  thumb_mime?: string;
  /** Пики звуковой волны (base64) — для отрисовки в UI. */
  waveform_base64?: string;
}

export interface Message {
  local_id: string;
  server_id: number | null;
  chat_id: string;
  sender_user_id: string;
  sender_device_id: string;
  content_kind: string;
  body: string | null;
  /** Метаданные вложения; содержимое лежит в ядре (скачивается отдельно). */
  attachment: AttachmentInfo | null;
  /** Идентификатор альбома: сообщения с одинаковым id — один альбом. */
  grouped_id?: string | null;
  reply_to_server_id: number | null;
  status: MessageStatus;
  created_at_ms: number;
  edited_at_ms: number | null;
  deleted_at_ms: number | null;
}

export interface Session {
  active: boolean;
  user_id: string;
  username: string;
  display_name: string;
  device_id: string;
  avatar: AvatarInfo;
}

export interface Receipt {
  chat_id: string;
  user_id: string;
  delivered_up_to: number;
  read_up_to: number;
  updated_at_ms: number;
}

export interface SyncProgress {
  from_seq: number;
  to_seq: number;
  events_applied: number;
  events_skipped: number;
  resync_performed: boolean;
}

export interface Call {
  call_id: string;
  chat_id: string;
  caller_user_id: string;
  callee_user_id: string;
  state: "ringing" | "active" | "unknown";
  created_at_ms: number;
  accepted_at_ms: number | null;
}

// ---------------------------------------------------------------------------
// Конверт запросов/ответов (§4)

export type ConnectionState =
  | "disconnected"
  | "connecting"
  | "unauthenticated"
  | "connected"
  | "reconnecting";

export interface OkEnvelope<T = unknown> {
  "@type": "ok";
  request: string;
  data: T;
}

export interface ErrorEnvelope {
  "@type": "error";
  request?: string;
  code: string;
  message: string;
  retryable: boolean;
  server_code?: number;
  server_code_name?: string;
}

export type ResponseEnvelope<T = unknown> = OkEnvelope<T> | ErrorEnvelope;

// ---------------------------------------------------------------------------
// События (§7)

export interface OutboxChange {
  local_id: string;
  state: Exclude<MessageStatus, "none">;
  error: string | null;
}

export type RchatEvent =
  | { event: "connection_state_changed"; data: { state: ConnectionState } }
  | { event: "session_started"; data: { user_id: string; username: string } }
  | { event: "session_ended"; data: { reason: string } }
  | { event: "message_received"; data: Message }
  | { event: "message_updated"; data: Message }
  | { event: "outbox_changed"; data: OutboxChange }
  | { event: "chat_upserted"; data: Chat }
  | { event: "receipt_updated"; data: Receipt }
  | {
      event: "private_mode_requested";
      data: {
        chat_id: string;
        requester_user_id: string;
        requester_username: string;
        requester_display_name: string;
      };
    }
  | { event: "private_mode_cancelled"; data: { chat_id: string } }
  | { event: "private_mode_resolved"; data: { chat_id: string; accepted: boolean } }
  | { event: "presence_changed"; data: { user_id: string; online: boolean } }
  | { event: "call_invited"; data: Call }
  | { event: "call_accepted"; data: Call }
  | { event: "call_declined"; data: Call }
  | { event: "call_ended"; data: Call }
  | { event: "sync_progress"; data: SyncProgress }
  | { event: "resync_performed"; data: { up_to_seq: number } }
  | {
      event: "e2e_decrypt_failed";
      data: { chat_id: string; server_id: number; device_id: string };
    }
  | { event: "warning"; data: { message: string } }
  | { event: "event_lagged"; data: { missed: number } }
  // Форвард-совместимость: неизвестные события не ломают клиент.
  | { event: string; data: unknown };

// ---------------------------------------------------------------------------
// Данные ответов, используемые UI

export interface SendMessageResult {
  local_id: string;
}

export interface MarkReadResult {
  up_to_server_id: number;
}

export interface LogoutResult {
  wiped: boolean;
}

export interface ConnectionStateResult {
  state: ConnectionState;
}

/**
 * Тексты сообщений для превью и цитаты (ответа).
 *
 * Служебные сообщения (карточки смены режима чата) приходят с пустым текстом
 * у старых сообщений, поэтому вид сообщения переводится в короткий текст —
 * как в Telegram, где служебное сообщение видно и в списке чатов, и в ответе
 * на него.
 */

/**
 * Пауза, которая разрывает серию сообщений одного автора (как в Telegram):
 * после неё у каждого пузыря свой хвостик, а не только у последнего.
 */
export const GROUP_GAP_MS = 5 * 60 * 1000;

/** Сообщение ещё в очереди отправки (иконка часов). */
export function isPendingStatus(status: string | undefined): boolean {
  return status === "pending" || status === "sending";
}

/**
 * Продолжение ли `next` серии сообщения `current`: тот же автор, тот же
 * день, пауза меньше `GROUP_GAP_MS` (и не альбом).
 *
 * Сообщения в очереди отправки (часы) не соединяются с уже отправленными —
 * у них своя серия, как в Telegram.
 */
export function isSameGroup(
  current: {
    sender_user_id: string;
    grouped_id?: string | null;
    created_at_ms: number;
    status?: string;
  },
  next: {
    sender_user_id: string;
    grouped_id?: string | null;
    created_at_ms: number;
    status?: string;
  }
): boolean {
  if (current.sender_user_id !== next.sender_user_id) return false;
  const grouped = (id: string | null | undefined): boolean =>
    id !== null && id !== undefined && id !== "";
  if (grouped(current.grouped_id) || grouped(next.grouped_id)) return false;
  const day = (ms: number): number => new Date(ms).setHours(0, 0, 0, 0);
  if (day(current.created_at_ms) !== day(next.created_at_ms)) return false;
  // «В очереди» и «отправлено» — разные серии.
  if (isPendingStatus(current.status) !== isPendingStatus(next.status)) {
    return false;
  }
  return next.created_at_ms - current.created_at_ms <= GROUP_GAP_MS;
}

/** Текст служебного сообщения по его виду (`null` — обычное сообщение). */
export function serviceMessageText(contentKind: string): string | null {
  switch (contentKind) {
    case "private_mode_request":
      return "Запрос на приватный режим";
    case "private_mode_disable":
      return "Предложение отключить приватный режим";
    default:
      return null;
  }
}

/** Текст сообщения: служебное — по виду, удалённое — по состоянию. */
export function messageText(message: {
  content_kind: string;
  body: string | null;
  deleted_at_ms: number | null;
}): string {
  if (message.deleted_at_ms !== null) return "Сообщение удалено";
  const service = serviceMessageText(message.content_kind);
  if (service !== null) return service;
  return message.body ?? "";
}

/** Служебное ли это сообщение (вид отличается от обычного текста). */
export function isServiceMessage(contentKind: string): boolean {
  return serviceMessageText(contentKind) !== null;
}

/**
 * Тексты сообщений для превью и цитаты (ответа).
 *
 * Служебные сообщения (карточки смены режима чата) приходят с пустым текстом
 * у старых сообщений, поэтому вид сообщения переводится в короткий текст —
 * как в Telegram, где служебное сообщение видно и в списке чатов, и в ответе
 * на него.
 */

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

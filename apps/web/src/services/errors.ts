/**
 * Классификация ошибок веб-клиента: из типизированного кода FFI/сервера
 * получаем понятное пользователю сообщение и подходящее действие.
 *
 * Правила:
 * - текст составляется по подтверждённому коду и контексту операции; из
 *   ошибки WebSocket или неизвестного исключения не делается вывод
 *   «нет интернета»;
 * - для неизвестной ошибки показывается безопасное общее сообщение без
 *   технических деталей (в них могли бы попасть токены, ключи или текст
 *   переписки);
 * - действие предлагается там, где оно применимо: повтор чтения, вход,
 *   другой файл, ожидание соединения.
 */

import { RchatApiError } from "./api";

/** Что именно не получилось. */
export type ErrorKind =
  /** Нет связи с локальным шлюзом (WebSocket закрыт или не открыт). */
  | "gateway_offline"
  /** Шлюз на месте, но ядро не связано с сервером. */
  | "server_offline"
  /** Операция не уложилась в время. */
  | "timeout"
  /** Сессия истекла или отозвана. */
  | "session_expired"
  /** Неверный логин или пароль. */
  | "invalid_credentials"
  /** Нет прав на операцию. */
  | "forbidden"
  /** Превышен лимит (частоты или размера). */
  | "rate_limited"
  /** Файл или сообщение слишком велико. */
  | "too_large"
  /** Нет ключей для расшифровки (или чата). */
  | "keys_unavailable"
  /** Некорректные данные. */
  | "invalid_input"
  /** Объект не найден. */
  | "not_found"
  /** Прочее — безопасное общее сообщение. */
  | "unknown";

/** Что пользователь может сделать. */
export type ErrorActionKind =
  /** Повторить операцию. */
  | "retry"
  /** Войти заново. */
  | "login"
  /** Выбрать другой файл. */
  | "choose_file"
  /** Дождаться соединения (оно восстановится само). */
  | "wait";

export interface ErrorAction {
  kind: ErrorActionKind;
  label: string;
}

/** Готовое к показу уведомление об ошибке. */
export interface ErrorNotice {
  kind: ErrorKind;
  /** Короткий заголовок. */
  title: string;
  /** Пояснение для пользователя. */
  message: string;
  /** Подходящее действие, если оно есть. */
  action?: ErrorAction;
  /** Имеет ли смысл повторять операцию. */
  retryable: boolean;
  /** Операция, во время которой возникла ошибка. */
  operation?: string;
}

/** Контекст операции — влияет на формулировку. */
export interface ErrorContext {
  /** Что делали: `send`, `login`, `history`, `attachment`, … */
  operation?: string;
  /** Чат, к которому относится ошибка. */
  chatId?: string;
  /** Размер файла (для «слишком большой»). */
  sizeBytes?: number;
}

/** Предел размещения вложения (совпадает с ядром). */
const MAX_ATTACHMENT_BYTES = 16 * 1024 * 1024;

/** Коды сервера (proto `ErrorCode`) — по именам, они стабильнее номеров. */
const SERVER_CODES = {
  invalid_credentials: "invalid_credentials",
  username_taken: "username_taken",
  username_invalid: "username_invalid",
  unauthenticated: "unauthenticated",
  session_expired: "session_expired",
  session_revoked: "session_revoked",
  forbidden: "forbidden",
  not_found: "not_found",
  conflict: "conflict",
  device_conflict: "device_conflict",
  rate_limited: "rate_limited",
  payload_too_large: "payload_too_large",
  e2e_required: "e2e_required",
  e2e_keys_missing: "e2e_keys_missing",
  limit_exceeded: "limit_exceeded",
  internal: "internal",
  resync_required: "resync_required",
} as const;

function notice(
  kind: ErrorKind,
  title: string,
  message: string,
  action?: ErrorAction,
  retryable = false,
  operation?: string
): ErrorNotice {
  const result: ErrorNotice = { kind, title, message, retryable };
  if (action !== undefined) result.action = action;
  if (operation !== undefined) result.operation = operation;
  return result;
}

/** Безопасное общее сообщение: без технических деталей. */
function unknownNotice(context: ErrorContext, retryable = true): ErrorNotice {
  const operation = context.operation;
  if (operation === "login") {
    return notice(
      "unknown",
      "Не удалось войти",
      "Повторите попытку. Если ошибка повторяется — проверьте логин и пароль.",
      { kind: "retry", label: "Повторить" },
      retryable,
      operation
    );
  }
  if (operation === "attachment") {
    return notice(
      "unknown",
      "Не удалось загрузить файл",
      "Попробуйте выбрать другой файл.",
      { kind: "choose_file", label: "Выбрать файл" },
      retryable,
      operation
    );
  }
  return notice(
    "unknown",
    "Что-то пошло не так",
    "Попробуйте повторить действие.",
    { kind: "retry", label: "Повторить" },
    retryable,
    operation
  );
}

/**
 * Превращает ошибку в уведомление: вид ошибки, текст и действие.
 * Никогда не бросает и всегда возвращает безопасный результат.
 */
export function classifyError(
  error: unknown,
  context: ErrorContext = {}
): ErrorNotice {
  if (!(error instanceof RchatApiError)) {
    // Неизвестное исключение (в том числе из браузера) — безопасное общее
    // сообщение: детали могут содержать чувствительные данные.
    return unknownNotice(context);
  }

  const serverCode = error.serverCodeName;
  const retry = error.retryable;
  // Известный код сервера: конкретное сообщение и действие.
  // Неизвестный код — безопасное общее сообщение с учётом retryable.

  // 1) Коды сервера важнее: FFI-код «server» сам по себе ничего не значит.
  if (error.code === "server" && serverCode !== undefined) {
    switch (serverCode) {
      case SERVER_CODES.invalid_credentials:
        return notice(
          "invalid_credentials",
          "Неверный логин или пароль",
          "Проверьте логин и пароль и попробуйте снова.",
          undefined,
          false,
          context.operation
        );
      case SERVER_CODES.username_taken:
        return notice(
          "invalid_input",
          "Имя уже занято",
          "Выберите другое имя пользователя.",
          undefined,
          false,
          context.operation
        );
      case SERVER_CODES.username_invalid:
        return notice(
          "invalid_input",
          "Недопустимое имя",
          "Имя может содержать латиницу, цифры, «_» и «-».",
          undefined,
          false,
          context.operation
        );
      case SERVER_CODES.unauthenticated:
      case SERVER_CODES.session_expired:
      case SERVER_CODES.session_revoked:
        return notice(
          "session_expired",
          "Сессия закончилась",
          "Войдите заново, чтобы продолжить.",
          { kind: "login", label: "Войти" },
          false,
          context.operation
        );
      case SERVER_CODES.forbidden:
        return notice(
          "forbidden",
          "Нет доступа",
          "У вас нет прав на это действие в данном чате.",
          undefined,
          false,
          context.operation
        );
      case SERVER_CODES.not_found:
        return notice(
          "not_found",
          "Не найдено",
          "Объект удалён или недоступен.",
          undefined,
          false,
          context.operation
        );
      case SERVER_CODES.conflict:
      case SERVER_CODES.device_conflict:
        return notice(
          "invalid_input",
          "Уже существует",
          "Такой чат или устройство уже есть.",
          undefined,
          false,
          context.operation
        );
      case SERVER_CODES.rate_limited:
      case SERVER_CODES.limit_exceeded:
        return notice(
          "rate_limited",
          "Слишком много запросов",
          "Подождите немного и повторите попытку.",
          { kind: "wait", label: "Подождать" },
          true,
          context.operation
        );
      case SERVER_CODES.payload_too_large:
        return tooLargeNotice(context);
      case SERVER_CODES.e2e_required:
      case SERVER_CODES.e2e_keys_missing:
        return keysNotice(context);
      case SERVER_CODES.resync_required:
        return notice(
          "unknown",
          "Нужно обновить историю",
          "Чаты будут перечитаны автоматически.",
          { kind: "retry", label: "Повторить" },
          true,
          context.operation
        );
      default:
        return unknownNotice(context, retry);
    }
  }

  // 2) Коды FFI.
  switch (error.code) {
    case "transport":
      return notice(
        "gateway_offline",
        "Нет связи с шлюзом",
        "Локальный шлюз недоступен. Соединение восстановится автоматически.",
        { kind: "wait", label: "Подождать" },
        true,
        context.operation
      );
    case "not_connected":
      return notice(
        "server_offline",
        "Нет связи с сервером",
        "Ядро ещё не подключилось к серверу. Повторите, когда соединение появится.",
        { kind: "wait", label: "Подождать" },
        true,
        context.operation
      );
    case "timeout":
      return notice(
        "timeout",
        "Не дождались ответа",
        "Операция заняла слишком много времени. Повторите попытку.",
        { kind: "retry", label: "Повторить" },
        true,
        context.operation
      );
    case "not_authenticated":
      return notice(
        "session_expired",
        "Сессия закончилась",
        "Войдите заново, чтобы продолжить.",
        { kind: "login", label: "Войти" },
        false,
        context.operation
      );
    case "e2e_unavailable":
    case "crypto":
      return keysNotice(context);
    case "invalid_request":
      return notice(
        "invalid_input",
        "Некорректный запрос",
        "Проверьте введённые данные и попробуйте снова.",
        undefined,
        false,
        context.operation
      );
    case "not_found":
      return notice(
        "not_found",
        "Не найдено",
        "Объект удалён или недоступен.",
        undefined,
        false,
        context.operation
      );
    case "closed":
      return notice(
        "gateway_offline",
        "Соединение закрыто",
        "Клиент остановлен. Переподключите его, чтобы продолжить.",
        { kind: "wait", label: "Подождать" },
        true,
        context.operation
      );
    default:
      // storage, secret_store, protocol, internal, panic и прочие — общее
      // безопасное сообщение без технических деталей.
      return unknownNotice(context, retry);
  }
}

function tooLargeNotice(context: ErrorContext): ErrorNotice {
  const limit = Math.floor(MAX_ATTACHMENT_BYTES / (1024 * 1024));
  const message =
    context.operation === "attachment"
      ? `Файл больше ${limit} МиБ — выберите другой.`
      : `Сообщение слишком велико (предел — ${limit} МиБ).`;
  return notice(
    "too_large",
    "Слишком большой файл",
    message,
    context.operation === "attachment"
      ? { kind: "choose_file", label: "Выбрать файл" }
      : undefined,
    false,
    context.operation
  );
}

function keysNotice(context: ErrorContext): ErrorNotice {
  return notice(
    "keys_unavailable",
    "Нет ключей для расшифровки",
    "Ключи ещё не получены. Подождите, пока собеседник пересоединится, и отправьте сообщение ещё раз.",
    { kind: "wait", label: "Подождать" },
    true,
    context.operation
  );
}

/**
 * Коды сервера по номерам (proto `ErrorCode`) — нужны для события очереди,
 * где приходит только числовой код.
 */
const SERVER_CODE_NUMBERS: Record<number, string> = {
  4: "invalid_credentials",
  5: "username_taken",
  6: "username_invalid",
  7: "unauthenticated",
  8: "session_expired",
  9: "session_revoked",
  10: "forbidden",
  11: "not_found",
  12: "conflict",
  13: "device_conflict",
  14: "rate_limited",
  15: "payload_too_large",
  16: "e2e_required",
  17: "e2e_keys_missing",
  18: "unsupported_capability",
  19: "internal",
  20: "resync_required",
  21: "limit_exceeded",
};

/**
 * Уведомление об ошибке отправки из очереди: сервер вернул код, но не
 * вернул готовый текст для пользователя.
 */
export function classifyOutboxFailure(
  errorCode: number | null | undefined,
  context: ErrorContext = {}
): ErrorNotice {
  if (errorCode === null || errorCode === undefined) {
    return unknownNotice(context);
  }
  const name = SERVER_CODE_NUMBERS[errorCode];
  if (name === undefined) {
    return unknownNotice(context);
  }
  return classifyError(
    new RchatApiError("server", "", false, errorCode, name),
    context
  );
}

/**
 * Ключ для сравнения: одинаковые уведомления не должны повторяться
 * (иначе получится поток одинаковых сообщений).
 */
export function noticeKey(notice: ErrorNotice): string {
  return `${notice.kind}|${notice.title}|${notice.message}`;
}

/** Текст уведомления для показа (заголовок + пояснение). */
export function noticeText(notice: ErrorNotice): string {
  return `${notice.title}: ${notice.message}`;
}

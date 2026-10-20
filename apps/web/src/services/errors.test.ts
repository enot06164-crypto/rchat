import { describe, expect, it } from "vitest";
import { RchatApiError } from "./api";
import {
  classifyError,
  classifyOutboxFailure,
  noticeKey,
} from "./errors";

/**
 * Проверки классификации ошибок: по подтверждённому коду получаем понятный
 * текст и подходящее действие; неизвестная ошибка даёт безопасное общее
 * сообщение без технических деталей.
 */

describe("классификация ошибок", () => {
  it("нет связи с шлюзом — это не «нет интернета», а ожидание соединения", () => {
    const notice = classifyError(
      new RchatApiError("transport", "шлюз не подключён", true),
      { operation: "send" }
    );
    expect(notice.kind).toBe("gateway_offline");
    expect(notice.title).toBe("Нет связи с шлюзом");
    expect(notice.message).not.toContain("интернет");
    expect(notice.action?.kind).toBe("wait");
  });

  it("нет связи ядра с сервером — отличаем от шлюза", () => {
    const notice = classifyError(
      new RchatApiError("not_connected", "not connected", true),
      { operation: "history" }
    );
    expect(notice.kind).toBe("server_offline");
    expect(notice.action?.kind).toBe("wait");
  });

  it("тайм-аут предлагает повторить", () => {
    const notice = classifyError(
      new RchatApiError("timeout", "timeout", true),
      { operation: "history" }
    );
    expect(notice.kind).toBe("timeout");
    expect(notice.action?.kind).toBe("retry");
    expect(notice.retryable).toBe(true);
  });

  it("истёкшая сессия предлагает войти", () => {
    const notice = classifyError(
      new RchatApiError("server", "session has expired", false, 8, "session_expired"),
      { operation: "send" }
    );
    expect(notice.kind).toBe("session_expired");
    expect(notice.action?.kind).toBe("login");
  });

  it("нет доступа — без действия повтора", () => {
    const notice = classifyError(
      new RchatApiError("server", "forbidden", false, 10, "forbidden"),
      { operation: "chat" }
    );
    expect(notice.kind).toBe("forbidden");
    expect(notice.action).toBeUndefined();
  });

  it("превышен лимит — подождать", () => {
    const notice = classifyError(
      new RchatApiError("server", "too many requests", true, 14, "rate_limited"),
      { operation: "send" }
    );
    expect(notice.kind).toBe("rate_limited");
    expect(notice.action?.kind).toBe("wait");
  });

  it("слишком большой файл — выбрать другой", () => {
    const notice = classifyError(
      new RchatApiError("server", "too large", false, 15, "payload_too_large"),
      { operation: "attachment" }
    );
    expect(notice.kind).toBe("too_large");
    expect(notice.action?.kind).toBe("choose_file");
    expect(notice.message).toContain("16 МиБ");
  });

  it("нет ключей расшифровки — подождать", () => {
    const notice = classifyError(
      new RchatApiError("server", "keys missing", false, 17, "e2e_keys_missing"),
      { operation: "send" }
    );
    expect(notice.kind).toBe("keys_unavailable");
    expect(notice.action?.kind).toBe("wait");
  });

  it("неверный пароль — без технических подробностей", () => {
    const notice = classifyError(
      new RchatApiError("server", "invalid credentials", false, 4, "invalid_credentials"),
      { operation: "login" }
    );
    expect(notice.kind).toBe("invalid_credentials");
    expect(notice.message).not.toContain("invalid credentials");
  });

  it("неизвестный код даёт безопасное общее сообщение", () => {
    const notice = classifyError(
      new RchatApiError("server", "sql: table users missing", false, 19, "internal"),
      { operation: "send" }
    );
    expect(notice.kind).toBe("unknown");
    expect(notice.message).not.toContain("sql");
    expect(notice.message).not.toContain("users");
  });

  it("неизвестное исключение не превращается в «нет интернета»", () => {
    const notice = classifyError(new Error("boom"), { operation: "send" });
    expect(notice.kind).toBe("unknown");
    expect(notice.message).not.toContain("интернет");
    expect(notice.message).not.toContain("boom");
  });

  it("ошибка очереди классифицируется по коду сервера", () => {
    const notice = classifyOutboxFailure(10, { operation: "send" });
    expect(notice.kind).toBe("forbidden");
  });

  it("ошибка очереди без кода — безопасное общее сообщение", () => {
    const notice = classifyOutboxFailure(null, { operation: "send" });
    expect(notice.kind).toBe("unknown");
  });

  it("одинаковые уведомления имеют одинаковый ключ (без потока одинаковых)", () => {
    const first = classifyError(new RchatApiError("timeout", "timeout", true));
    const second = classifyError(new RchatApiError("timeout", "другое", true));
    expect(noticeKey(first)).toBe(noticeKey(second));
  });
});

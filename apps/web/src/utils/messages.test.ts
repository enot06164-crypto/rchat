import { describe, expect, it } from "vitest";
import {
  GROUP_GAP_MS,
  isSameGroup,
  isServiceMessage,
  messageText,
  serviceMessageText,
} from "./messages";

/**
 * Служебные сообщения (карточки смены режима чата) приходят с пустым текстом:
 * в превью списка чатов и в ответе на сообщение не должно быть пустоты.
 */
describe("текст служебного сообщения", () => {
  it("у карточек есть свой текст", () => {
    expect(serviceMessageText("private_mode_request")).toBe(
      "Запрос на приватный режим"
    );
    expect(serviceMessageText("private_mode_disable")).toBe(
      "Предложение отключить приватный режим"
    );
    expect(serviceMessageText("text")).toBeNull();
  });

  it("служебные сообщения опознаются по виду", () => {
    expect(isServiceMessage("private_mode_request")).toBe(true);
    expect(isServiceMessage("private_mode_disable")).toBe(true);
    expect(isServiceMessage("text")).toBe(false);
    expect(isServiceMessage("photo")).toBe(false);
  });

  it("в превью и цитате пустого служебного сообщения видно текст карточки", () => {
    const service = {
      content_kind: "private_mode_request",
      body: "",
      deleted_at_ms: null,
    };
    expect(messageText(service)).toBe("Запрос на приватный режим");
  });

  it("обычный текст не меняется", () => {
    expect(
      messageText({ content_kind: "text", body: "привет", deleted_at_ms: null })
    ).toBe("привет");
  });

  it("удалённое сообщение помечается", () => {
    expect(
      messageText({
        content_kind: "text",
        body: "привет",
        deleted_at_ms: 1_700_000_000_000,
      })
    ).toBe("Сообщение удалено");
  });
});

/**
 * Серия сообщений одного автора: пока пауза меньше пяти минут, сообщения
 * идут одной серией (хвостик — только у последнего).  Большая пауза разрывает
 * серию — тогда у каждого пузыря свой хвостик, как в Telegram.
 */
describe("серия сообщений", () => {
  const base = {
    sender_user_id: "u1",
    grouped_id: null,
    created_at_ms: new Date("2026-01-10T12:00:00").getTime(),
  };

  it("сообщения подряд — одна серия", () => {
    expect(isSameGroup(base, { ...base, created_at_ms: base.created_at_ms + 1000 })).toBe(true);
  });

  it("пауза больше пяти минут разрывает серию", () => {
    expect(
      isSameGroup(base, {
        ...base,
        created_at_ms: base.created_at_ms + GROUP_GAP_MS + 1,
      })
    ).toBe(false);
  });

  it("пауза ровно в пять минут — ещё серия", () => {
    expect(
      isSameGroup(base, {
        ...base,
        created_at_ms: base.created_at_ms + GROUP_GAP_MS,
      })
    ).toBe(true);
  });

  it("разные авторы — разные серии", () => {
    expect(
      isSameGroup(base, {
        ...base,
        sender_user_id: "u2",
        created_at_ms: base.created_at_ms + 1000,
      })
    ).toBe(false);
  });

  it("разные дни — разные серии", () => {
    const tomorrow = base.created_at_ms + 24 * 60 * 60 * 1000;
    expect(
      isSameGroup(base, { ...base, created_at_ms: tomorrow })
    ).toBe(false);
  });

  it("альбом не присоединяется к серии", () => {
    expect(
      isSameGroup(base, {
        ...base,
        grouped_id: "g-1",
        created_at_ms: base.created_at_ms + 1000,
      })
    ).toBe(false);
  });
});

import { describe, expect, it } from "vitest";
import {
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

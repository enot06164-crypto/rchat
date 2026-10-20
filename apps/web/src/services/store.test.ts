import { beforeEach, describe, expect, it } from "vitest";
import { applyEvent, setState, state } from "../services/store";
import type { Message } from "../types/rchat";

/**
 * Регрессия: в облачном чате сервер зеркалит отправителю его сообщение уже
 * с текстом (он его расшифровал), а в приватном — без текста. Раньше веб
 * отбрасывал дубль только по пустому тексту, поэтому в облачном чате
 * отправленное сообщение показывалось дважды.
 */

const CHAT_ID = "c1";
const ME = "u-me";

function message(overrides: Partial<Message>): Message {
  return {
    local_id: "s:10",
    server_id: 10,
    chat_id: CHAT_ID,
    sender_user_id: ME,
    sender_device_id: "d1",
    content_kind: "text",
    body: "ФЫВ",
    attachment: null,
    reply_to_server_id: null,
    status: "sent",
    created_at_ms: 1_700_000_000_000,
    edited_at_ms: null,
    deleted_at_ms: null,
    ...overrides,
  };
}

describe("своё отправленное сообщение", () => {
  beforeEach(() => {
    setState({
      messages: {},
      chats: [],
      receipts: {},
      members: {},
      session: null,
      unread: {},
      replyTo: null,
    });
  });

  it("не дублируется в облачном чате (эхо с текстом)", () => {
    // Оптимистичная копия: локальный id клиента, серверного id ещё нет.
    applyEvent({
      event: "message_updated",
      data: message({
        local_id: "client-1",
        server_id: null,
        body: "ФЫВ",
        status: "sending",
      }),
    });
    // Подтверждение отправки: ядро знает server_id.
    applyEvent({
      event: "message_updated",
      data: message({ local_id: "client-1", server_id: 10, status: "sent" }),
    });
    // Эхо сервера: другой local_id ("s:<server_id>"), текст уже есть.
    applyEvent({
      event: "message_received",
      data: message({ local_id: "s:10", server_id: 10, body: "ФЫВ" }),
    });

    const list = state.messages[CHAT_ID] ?? [];
    expect(list).toHaveLength(1);
    expect(list[0].body).toBe("ФЫВ");
    expect(list[0].status).toBe("sent");
  });

  it("не дублируется в приватном чате (эхо без текста) и текст не теряется", () => {
    applyEvent({
      event: "message_updated",
      data: message({
        local_id: "client-1",
        server_id: null,
        body: "ФЫВ",
        status: "sending",
      }),
    });
    applyEvent({
      event: "message_updated",
      data: message({ local_id: "client-1", server_id: 10, status: "sent" }),
    });
    // В приватном чате у отправителя нет своей копии ciphertext — эхо без текста.
    applyEvent({
      event: "message_received",
      data: message({ local_id: "s:10", server_id: 10, body: null }),
    });

    const list = state.messages[CHAT_ID] ?? [];
    expect(list).toHaveLength(1);
    expect(list[0].body).toBe("ФЫВ");
  });

  it("чужие сообщения добавляются как есть", () => {
    applyEvent({
      event: "message_received",
      data: message({
        local_id: "s:11",
        server_id: 11,
        sender_user_id: "u-other",
        body: "привет",
      }),
    });
    const list = state.messages[CHAT_ID] ?? [];
    expect(list).toHaveLength(1);
    expect(list[0].body).toBe("привет");
  });
});

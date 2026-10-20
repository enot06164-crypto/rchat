import { beforeEach, describe, expect, it } from "vitest";
import { applyEvent, setState, state } from "../services/store";
import type { Message } from "../types/rchat";

/**
 * Регрессия: в облачном чате сервер зеркалит отправителю его сообщение уже
 * с текстом (он его расшифровал), а в приватном — без текста. Раньше веб
 * отбрасывал дубль только по пустому тексту, поэтому в облачном чате
 * отправленное сообщение показывалось дважды.
 */

const PROFILE = "default";
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

function messages(): Message[] {
  return state.accounts[PROFILE]?.messages[CHAT_ID] ?? [];
}

describe("своё отправленное сообщение", () => {
  beforeEach(() => {
    setState("accounts", PROFILE, {
      session: null,
      chats: [],
      messages: {},
      receipts: {},
      members: {},
      activeChatId: null,
      unread: {},
      replyTo: null,
      booted: true,
      busy: false,
      authError: null,
      sendError: null,
      newChatError: null,
      defaultChatMode: "cloud",
      searchHits: [],
      searchBusy: false,
      presence: {},
    });
  });

  it("не дублируется в облачном чате (эхо с текстом)", () => {
    // Оптимистичная копия: локальный id клиента, серверного id ещё нет.
    applyEvent(PROFILE, {
      event: "message_updated",
      data: message({
        local_id: "client-1",
        server_id: null,
        body: "ФЫВ",
        status: "sending",
      }),
    });
    // Подтверждение отправки: ядро знает server_id.
    applyEvent(PROFILE, {
      event: "message_updated",
      data: message({ local_id: "client-1", server_id: 10, status: "sent" }),
    });
    // Эхо сервера: другой local_id ("s:<server_id>"), текст уже есть.
    applyEvent(PROFILE, {
      event: "message_received",
      data: message({ local_id: "s:10", server_id: 10, body: "ФЫВ" }),
    });

    expect(messages()).toHaveLength(1);
    expect(messages()[0].body).toBe("ФЫВ");
    expect(messages()[0].status).toBe("sent");
  });

  it("не дублируется в приватном чате (эхо без текста) и текст не теряется", () => {
    applyEvent(PROFILE, {
      event: "message_updated",
      data: message({
        local_id: "client-1",
        server_id: null,
        body: "ФЫВ",
        status: "sending",
      }),
    });
    applyEvent(PROFILE, {
      event: "message_updated",
      data: message({ local_id: "client-1", server_id: 10, status: "sent" }),
    });
    // В приватном чате у отправителя нет своей копии ciphertext — эхо без текста.
    applyEvent(PROFILE, {
      event: "message_received",
      data: message({ local_id: "s:10", server_id: 10, body: null }),
    });

    expect(messages()).toHaveLength(1);
    expect(messages()[0].body).toBe("ФЫВ");
  });

  it("чужие сообщения добавляются как есть", () => {
    applyEvent(PROFILE, {
      event: "message_received",
      data: message({
        local_id: "s:11",
        server_id: 11,
        sender_user_id: "u-other",
        body: "привет",
      }),
    });
    expect(messages()).toHaveLength(1);
    expect(messages()[0].body).toBe("привет");
  });

  it("аккаунты не смешиваются: события идут в свой слот", () => {
    applyEvent("alice", {
      event: "message_received",
      data: message({ local_id: "s:12", server_id: 12, body: "у Алисы" }),
    });
    applyEvent("bob", {
      event: "message_received",
      data: message({ local_id: "s:13", server_id: 13, body: "у Боба" }),
    });
    expect(state.accounts["alice"]?.messages[CHAT_ID]).toHaveLength(1);
    expect(state.accounts["alice"]?.messages[CHAT_ID]?.[0].body).toBe("у Алисы");
    expect(state.accounts["bob"]?.messages[CHAT_ID]).toHaveLength(1);
    expect(state.accounts["bob"]?.messages[CHAT_ID]?.[0].body).toBe("у Боба");
  });
});

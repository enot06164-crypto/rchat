import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Message } from "../types/rchat";

/**
 * Переход к сообщению по ответу: если сообщения нет в загруженной ленте,
 * веб подгружает более старые страницы истории.
 */

const { fetchHistory } = vi.hoisted(() => ({ fetchHistory: vi.fn() }));

vi.mock("../services/api", () => ({
  apiFor: () => ({}),
  chatApiFor: () => ({ fetchHistory }),
  closeProfile: vi.fn(),
  listProfiles: vi.fn(() => []),
}));

const { ensureMessageLoaded, loadOlderHistory, setState } = await import(
  "../services/store"
);

const PROFILE = "default";
const CHAT_ID = "c1";

function message(overrides: Partial<Message>): Message {
  return {
    local_id: "s:10",
    server_id: 10,
    chat_id: CHAT_ID,
    sender_user_id: "u-other",
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

describe("подгрузка истории для перехода к сообщению", () => {
  beforeEach(() => {
    fetchHistory.mockReset();
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
      privateModeRequests: {},
    });
  });

  it("сообщение, которое уже в ленте, не требует загрузки", async () => {
    setState("accounts", PROFILE, "messages", CHAT_ID, [
      message({ local_id: "s:10", server_id: 10 }),
    ]);
    const found = await ensureMessageLoaded(PROFILE, CHAT_ID, 10);
    expect(found).toBe(true);
    expect(fetchHistory).not.toHaveBeenCalled();
  });

  it("недостающее сообщение ищется в более старых страницах", async () => {
    setState("accounts", PROFILE, "messages", CHAT_ID, [
      message({ local_id: "s:10", server_id: 10 }),
    ]);
    fetchHistory.mockResolvedValueOnce([
      message({ local_id: "s:4", server_id: 4, body: "старое" }),
    ]);

    const found = await ensureMessageLoaded(PROFILE, CHAT_ID, 4);
    expect(found).toBe(true);
    // Страница запрашивается перед самым старым известным сообщением.
    expect(fetchHistory).toHaveBeenCalledWith(CHAT_ID, 10, 100);
  });

  it("если сообщения нет — подгрузка прекращается", async () => {
    setState("accounts", PROFILE, "messages", CHAT_ID, [
      message({ local_id: "s:10", server_id: 10 }),
    ]);
    fetchHistory.mockResolvedValue([]);

    const found = await ensureMessageLoaded(PROFILE, CHAT_ID, 4);
    expect(found).toBe(false);
    expect(fetchHistory).toHaveBeenCalledTimes(1);
  });

  it("подгруженная страница попадает в ленту", async () => {
    setState("accounts", PROFILE, "messages", CHAT_ID, [
      message({ local_id: "s:10", server_id: 10 }),
    ]);
    fetchHistory.mockResolvedValueOnce([
      message({ local_id: "s:4", server_id: 4, body: "старое" }),
    ]);

    await loadOlderHistory(PROFILE, CHAT_ID, 10);
    const list = (await import("../services/store")).state.accounts[PROFILE]
      ?.messages[CHAT_ID];
    expect(list?.map((item) => item.server_id)).toContain(4);
  });
});

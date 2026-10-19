import { Show, createMemo } from "solid-js";
import { state } from "../../services/store";
import { Avatar } from "../Avatar";
import { MessageList } from "./MessageList";
import { MessageInput } from "./MessageInput";

/** Правая колонка: шапка чата, лента сообщений и поле ввода. */
export function ChatView() {
  const chat = createMemo(
    () => state.chats.find((c) => c.chat_id === state.activeChatId) ?? null
  );

  const subtitle = () => {
    const current = chat();
    if (current === null) return "";
    if (current.kind === "group") {
      const memberCount = Object.keys(state.members[current.chat_id] ?? {}).length;
      const base = memberCount > 0 ? `группа · ${memberCount} участн.` : "группа";
      return current.e2e_mode === "olm_v1" ? `${base} · E2E` : base;
    }
    return current.e2e_mode === "olm_v1" ? "сквозное шифрование" : "обычный чат";
  };

  return (
    <Show
      when={chat()}
      fallback={
        <div class="chat-view chat-view--empty">
          <div class="chat-view__placeholder">
            Выберите чат слева или создайте новый кнопкой «+».
          </div>
        </div>
      }
      keyed
    >
      {(current) => (
        <section class="chat-view">
          <header class="chat-view__header">
            <Avatar info={current.avatar} size={40} />
            <div class="chat-view__title-col">
              <span class="chat-view__title">{current.title}</span>
              <span class="chat-view__subtitle">{subtitle()}</span>
            </div>
          </header>

          <MessageList chatId={current.chat_id} />

          <MessageInput chatId={current.chat_id} />
        </section>
      )}
    </Show>
  );
}

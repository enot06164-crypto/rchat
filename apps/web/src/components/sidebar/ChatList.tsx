import { For, Show } from "solid-js";
import type { Chat } from "../../types/rchat";
import { state } from "../../services/store";
import { ChatItem } from "./ChatItem";

/** Прокрутка диалогов с фильтрами: вкладки «Все»/«Непрочитанные» и поиск. */
export function ChatList(props: { tab: "all" | "unread"; query: string }) {
  const chats = (): Chat[] => {
    let list = state.chats;
    if (props.tab === "unread") {
      list = list.filter(
        (chat) => (state.unread[chat.chat_id] ?? 0) > 0
      );
    }
    const query = props.query.trim().toLowerCase();
    if (query !== "") {
      list = list.filter((chat) =>
        chat.title.toLowerCase().includes(query)
      );
    }
    return list;
  };

  return (
    <div class="chat-list">
      <Show
        when={chats().length > 0}
        fallback={
          <div class="chat-list__empty">
            {state.chats.length === 0
              ? "Нет чатов. Нажмите фиолетовый карандаш, чтобы начать диалог."
              : "Ничего не найдено."}
          </div>
        }
      >
        <For each={chats()}>{(chat) => <ChatItem chat={chat} />}</For>
      </Show>
    </div>
  );
}

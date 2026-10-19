import type { JSX } from "solid-js";
import type { Chat } from "../../types/rchat";
import { openChat, state } from "../../services/store";
import { Avatar } from "../Avatar";
import { formatShortTime } from "../../utils/format";

/** Элемент списка диалогов: аватар, название, превью, время, счётчик. */
export function ChatItem(props: { chat: Chat }): JSX.Element {
  const isActive = () => state.activeChatId === props.chat.chat_id;
  const unread = () => state.unread[props.chat.chat_id] ?? 0;
  const lastMessage = () => {
    const list = state.messages[props.chat.chat_id];
    if (list === undefined || list.length === 0) return undefined;
    return list[list.length - 1];
  };
  const preview = (): string => {
    const last = lastMessage();
    if (last === undefined) {
      return props.chat.kind === "group" ? "Групповой чат" : "Нет сообщений";
    }
    const prefix =
      state.session !== null && last.sender_user_id === state.session.user_id
        ? "Вы: "
        : "";
    const body =
      last.deleted_at_ms !== null ? "сообщение удалено" : (last.body ?? "");
    return `${prefix}${body}`;
  };

  const activate = (): void => {
    void openChat(props.chat.chat_id);
  };

  return (
    <div
      classList={{ "chat-item": true, "chat-item--active": isActive() }}
      role="button"
      tabIndex={0}
      onClick={activate}
      onKeyDown={(event) => {
        if (event.key === "Enter") activate();
      }}
    >
      <Avatar info={props.chat.avatar} size={48} />
      <div class="chat-item__body">
        <div class="chat-item__top">
          <span class="chat-item__title">{props.chat.title}</span>
          <span class="chat-item__time">
            {formatShortTime(props.chat.updated_at_ms)}
          </span>
        </div>
        <div class="chat-item__bottom">
          <span class="chat-item__preview">{preview()}</span>
          {unread() > 0 && <span class="chat-item__badge">{unread()}</span>}
        </div>
      </div>
    </div>
  );
}

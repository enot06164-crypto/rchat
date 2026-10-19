import { Match, Show, Switch } from "solid-js";
import type { JSX } from "solid-js";
import { Check, CheckCheck, CircleAlert, Clock, Lock } from "lucide-solid";
import type { Chat, MessageStatus } from "../../types/rchat";
import { openChat, readUpToFor, state } from "../../services/store";
import { Avatar } from "../Avatar";
import { formatShortTime } from "../../utils/format";

/** Иконка статуса исходящего превью (часы/галочка/двойная/ошибка). */
function PreviewStatus(props: { status: MessageStatus; serverId: number | null; readUpTo: number }) {
  const read = () => props.serverId !== null && props.serverId <= props.readUpTo;
  return (
    <Switch fallback={null}>
      <Match when={props.status === "pending" || props.status === "sending"}>
        <Clock size={14} />
      </Match>
      <Match when={props.status === "failed"}>
        <CircleAlert size={14} class="status-failed" />
      </Match>
      <Match when={props.status === "sent" && read()}>
        <CheckCheck size={15} class="status-read" />
      </Match>
      <Match when={props.status === "sent"}>
        <Check size={15} />
      </Match>
    </Switch>
  );
}

/** Элемент списка чатов (72px): аватар 54, имя, время, превью, бейдж. */
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
    const body =
      last.deleted_at_ms !== null ? "Сообщение удалено" : (last.body ?? "");
    return body;
  };
  const previewPrefix = (): string => {
    const last = lastMessage();
    if (last === undefined) return "";
    return state.session !== null && last.sender_user_id === state.session.user_id
      ? "Вы: "
      : "";
  };
  const previewStatus = () => {
    const last = lastMessage();
    if (last === undefined) return undefined;
    if (state.session === null || last.sender_user_id !== state.session.user_id) {
      return undefined;
    }
    return last;
  };

  // Имя отправителя в превью (в группах — как в Telegram, цветом палитры ядра).
  const senderLabel = () => {
    const last = lastMessage();
    if (last === undefined) return null;
    if (props.chat.kind !== "group") return null;
    if (state.session !== null && last.sender_user_id === state.session.user_id) {
      return null;
    }
    const member = state.members[props.chat.chat_id]?.[last.sender_user_id];
    if (member === undefined) return null;
    return {
      name: member.display_name || member.username,
      color: member.avatar.color_hex,
    };
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
      <Avatar info={props.chat.avatar} size={54} />
      <div class="chat-item__body">
        <div class="chat-item__top">
          <span class="chat-item__title">
            {/* Замок — приватный чат (E2E); облачные чаты, как в Telegram, без значка. */}
            <Show when={props.chat.privacy === "private"}>
              <Lock size={13} class="chat-item__lock" />
            </Show>
            {props.chat.title}
          </span>
          <span class="chat-item__time">
            {formatShortTime(props.chat.updated_at_ms)}
          </span>
        </div>
        <div class="chat-item__bottom">
          <span class="chat-item__preview">
            <Show when={previewStatus() !== undefined}>
              <PreviewStatus
                status={previewStatus()!.status}
                serverId={previewStatus()!.server_id}
                readUpTo={readUpToFor(props.chat.chat_id)}
              />
            </Show>
            <Show when={senderLabel() !== null}>
              <span
                class="chat-item__sender"
                style={{ color: senderLabel()!.color }}
              >
                {senderLabel()!.name}
              </span>
              {": "}
            </Show>
            {previewPrefix()}
            {preview()}
          </span>
          <Show when={unread() > 0}>
            <span class="chat-item__badge">{unread()}</span>
          </Show>
        </div>
      </div>
    </div>
  );
}

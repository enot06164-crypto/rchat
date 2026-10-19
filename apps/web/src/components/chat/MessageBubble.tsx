import { Match, Show, Switch } from "solid-js";
import { Check, CheckCheck, CircleAlert, Clock } from "lucide-solid";
import type { Message, MessageStatus } from "../../types/rchat";
import { state, readUpToFor } from "../../services/store";
import { Avatar } from "../Avatar";
import { formatTime } from "../../utils/format";

/**
 * Пузырь сообщения: хвостик-скобка, время и статус отправки для исходящих
 * (часы — в пути, галочка — отправлено, двойная — прочитано, восклицание —
 * ошибка отправки).
 */
export function MessageBubble(props: { message: Message }) {
  const own = () =>
    state.session !== null &&
    props.message.sender_user_id === state.session.user_id;
  const deleted = () => props.message.deleted_at_ms !== null;
  const body = () =>
    deleted() ? "Сообщение удалено" : (props.message.body ?? "");

  const senderAvatar = () => {
    const members = state.members[props.message.chat_id];
    const member = members?.[props.message.sender_user_id];
    if (member !== undefined) return member.avatar;
    const chat = state.chats.find((c) => c.chat_id === props.message.chat_id);
    return chat?.avatar;
  };

  return (
    <div
      classList={{
        "bubble-row": true,
        "bubble-row--own": own(),
        "bubble-row--in": !own(),
      }}
    >
      {!own() && <Avatar info={senderAvatar()} size={34} class="bubble-row__avatar" />}
      <div
        classList={{
          bubble: true,
          "bubble--own": own(),
          "bubble--in": !own(),
          "bubble--deleted": deleted(),
        }}
      >
        <div class="bubble__body">{body()}</div>
        <div class="bubble__meta">
          <span class="bubble__time">{formatTime(props.message.created_at_ms)}</span>
          <Show when={own() && !deleted()}>
            <span class="bubble__status">
              <StatusIcon
                status={props.message.status}
                serverId={props.message.server_id}
                readUpTo={readUpToFor(props.message.chat_id)}
              />
            </span>
          </Show>
        </div>
        <svg
          class="bubble__tail"
          width="9"
          height="20"
          viewBox="0 0 9 20"
          xmlns="http://www.w3.org/2000/svg"
          aria-hidden="true"
        >
          <path d="M0 0 C0 8 1 13 4 16.5 C5.3 18 6.8 19.2 9 20 L0 20 Z" />
        </svg>
      </div>
    </div>
  );
}

function StatusIcon(props: {
  status: MessageStatus;
  serverId: number | null;
  readUpTo: number;
}) {
  const read = () =>
    props.serverId !== null && props.serverId <= props.readUpTo;
  return (
    <Switch
      fallback={
        // status "none" у входящих — иконка не нужна
        null
      }
    >
      <Match when={props.status === "pending" || props.status === "sending"}>
        <Clock size={13} />
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

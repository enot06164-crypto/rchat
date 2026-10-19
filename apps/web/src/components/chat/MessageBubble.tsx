import { Match, Show, Switch, createSignal, onCleanup } from "solid-js";
import {
  Check,
  CheckCheck,
  CircleAlert,
  Clock,
  Copy,
  Forward,
  Reply,
  Trash2,
} from "lucide-solid";
import type { Message, MessageStatus } from "../../types/rchat";
import {
  deleteMessage,
  readUpToFor,
  setReplyTo,
  state,
} from "../../services/store";
import { Avatar } from "../Avatar";
import { formatTime } from "../../utils/format";

/** Ссылка на цитируемое сообщение (поиск по server_id в ленте чата). */
function findQuoted(message: Message): Message | undefined {
  const quotedId = message.reply_to_server_id;
  if (quotedId === null) return undefined;
  const list = state.messages[message.chat_id] ?? [];
  return list.find((m) => m.server_id === quotedId);
}

/** Псевдоним автора сообщения: «Вы» либо отображаемое имя участника/чата. */
function senderName(senderId: string, chatId: string): string {
  if (state.session !== null && senderId === state.session.user_id) {
    return "Вы";
  }
  const member = state.members[chatId]?.[senderId];
  if (member !== undefined) return member.display_name || member.username;
  const chat = state.chats.find((c) => c.chat_id === chatId);
  return chat?.title ?? "Неизвестно";
}

/** Цвет имени автора: палитра ядра (avatar.color_hex). */
function senderColor(senderId: string, chatId: string): string {
  const member = state.members[chatId]?.[senderId];
  if (member !== undefined && member.avatar.color_hex !== "") {
    return member.avatar.color_hex;
  }
  const chat = state.chats.find((c) => c.chat_id === chatId);
  return chat?.avatar.color_hex ?? "#8774e1";
}

interface MenuPos {
  x: number;
  y: number;
}

/**
 * Пузырь сообщения Amethyst Dark: входящие #18181b с хвостом слева и тонким
 * бордером, исходящие — градиент #2b2140→#3d2e5c с хвостом справа; реплай —
 * цитата с фиолетовой полосой; ПКМ открывает контекстное меню.
 */
export function MessageBubble(props: {
  message: Message;
  showAuthor: boolean;
  isGroup: boolean;
}) {
  const own = () =>
    state.session !== null &&
    props.message.sender_user_id === state.session.user_id;
  const deleted = () => props.message.deleted_at_ms !== null;
  const body = () =>
    deleted() ? "Сообщение удалено" : (props.message.body ?? "");
  const quoted = () => findQuoted(props.message);

  const senderAvatar = () => {
    const members = state.members[props.message.chat_id];
    const member = members?.[props.message.sender_user_id];
    if (member !== undefined) return member.avatar;
    const chat = state.chats.find((c) => c.chat_id === props.message.chat_id);
    return chat?.avatar;
  };

  // Контекстное меню (ПКМ по пузырю); закрывается кликом вне меню.
  const [menuPos, setMenuPos] = createSignal<MenuPos | null>(null);
  const openMenu = (event: MouseEvent): void => {
    event.preventDefault();
    // Не даём меню вылезти за края окна (≈210×176 px).
    setMenuPos({
      x: Math.min(event.clientX, window.innerWidth - 218),
      y: Math.min(event.clientY, window.innerHeight - 184),
    });
  };
  const closeMenu = (event: MouseEvent): void => {
    const target = event.target as HTMLElement | null;
    if (target === null || target.closest(".context-menu") === null) {
      setMenuPos(null);
    }
  };
  document.addEventListener("mousedown", closeMenu);
  onCleanup(() => document.removeEventListener("mousedown", closeMenu));

  const copyBody = (): void => {
    if (props.message.body !== null && navigator.clipboard !== undefined) {
      navigator.clipboard.writeText(props.message.body).catch(() => undefined);
    }
    setMenuPos(null);
  };

  return (
    <div
      classList={{
        "bubble-row": true,
        "bubble-row--own": own(),
        "bubble-row--in": !own(),
      }}
    >
      <Show when={!own()}>
        <Avatar info={senderAvatar()} size={34} class="bubble-row__avatar" />
      </Show>
      <div
        classList={{
          bubble: true,
          "bubble--own": own(),
          "bubble--in": !own(),
          "bubble--deleted": deleted(),
        }}
        onContextMenu={openMenu}
      >
        <Show when={!own() && props.isGroup && props.showAuthor}>
          <span
            class="bubble__author"
            style={{
              color: senderColor(
                props.message.sender_user_id,
                props.message.chat_id
              ),
            }}
          >
            {senderName(props.message.sender_user_id, props.message.chat_id)}
          </span>
        </Show>

        <Show when={!deleted() && quoted() !== undefined}>
          <div class="bubble__quote">
            <span
              class="bubble__quote-name"
              style={{
                color: senderColor(
                  quoted()!.sender_user_id,
                  quoted()!.chat_id
                ),
              }}
            >
              {senderName(quoted()!.sender_user_id, quoted()!.chat_id)}
            </span>
            <span class="bubble__quote-text">{quoted()!.body ?? ""}</span>
          </div>
        </Show>

        <div class="bubble__body">{body()}</div>
        <div class="bubble__meta">
          <span class="bubble__time">
            {formatTime(props.message.created_at_ms)}
          </span>
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

      <Show when={menuPos() !== null}>
        <div
          class="context-menu"
          style={{
            left: `${menuPos()!.x}px`,
            top: `${menuPos()!.y}px`,
          }}
          role="menu"
        >
          <button
            role="menuitem"
            class="context-menu__item"
            onClick={() => {
              setReplyTo(props.message);
              setMenuPos(null);
            }}
          >
            <Reply size={17} />
            Ответить
          </button>
          <button
            role="menuitem"
            class="context-menu__item"
            onClick={copyBody}
          >
            <Copy size={17} />
            Копировать текст
          </button>
          <button
            role="menuitem"
            class="context-menu__item"
            disabled
            title="Пересылка появится позже"
          >
            <Forward size={17} />
            Переслать
          </button>
          <Show
            when={
              own() &&
              props.message.server_id !== null &&
              props.message.deleted_at_ms === null
            }
          >
            <button
              role="menuitem"
              class="context-menu__item context-menu__item--danger"
              onClick={() => {
                setMenuPos(null);
                void deleteMessage(
                  props.message.chat_id,
                  props.message.server_id ?? 0
                );
              }}
            >
              <Trash2 size={17} />
              Удалить
            </button>
          </Show>
        </div>
      </Show>
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

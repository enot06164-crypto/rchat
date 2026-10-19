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
 * Пузырь сообщения в стиле Telegram WebK (Amethyst Dark):
 * - Группировка последовательных сообщений одного автора (`.bubble--group-first`,
 *   `.bubble--group-last`) со скруглениями 15px / 5px (по мотивам WebK).
 * - Собственный SVG-хвостик (`11×20`) у последнего сообщения группы: угол
 *   пузыря на стороне хвостика прямой, скругление даёт сам хвостик.
 * - Обтекаемый штамп времени и галочек (`.bubble__meta`) внутри `.bubble__body`.
 * - Двойной клик — быстрый реплай; ПКМ — контекстное меню с `backdrop-filter: blur(25px)`.
 */
export function MessageBubble(props: {
  message: Message;
  showAuthor: boolean;
  isGroup: boolean;
  isFirstInGroup?: boolean;
  isLastInGroup?: boolean;
}) {
  const own = () =>
    state.session !== null &&
    props.message.sender_user_id === state.session.user_id;
  const deleted = () => props.message.deleted_at_ms !== null;
  const body = () =>
    deleted() ? "Сообщение удалено" : (props.message.body ?? "");
  const quoted = () => findQuoted(props.message);
  const firstInGroup = () => props.isFirstInGroup ?? true;
  const lastInGroup = () => props.isLastInGroup ?? true;

  const senderAvatar = () => {
    const members = state.members[props.message.chat_id];
    const member = members?.[props.message.sender_user_id];
    if (member !== undefined) return member.avatar;
    const chat = state.chats.find((c) => c.chat_id === props.message.chat_id);
    return chat?.avatar;
  };

  // Контекстное меню (ПКМ по пузырю); закрывается кликом вне меню.
  const [menuPos, setMenuPos] = createSignal<MenuPos | null>(null);

  // Кнопка быстрого ответа показывается при наведении на пузырь. Кнопка живёт
  // за пределами пузыря, поэтому наведения на строку недостаточно: курсор
  // на долю секунды покидает пузырь, :hover теряется, кнопка гаснет и уже не
  // ловит курсор. Поэтому показом управляем из JS с задержкой скрытия.
  const [hovered, setHovered] = createSignal(false);
  let hideTimer: ReturnType<typeof setTimeout> | undefined;

  const showReply = (): void => {
    if (hideTimer !== undefined) {
      clearTimeout(hideTimer);
      hideTimer = undefined;
    }
    setHovered(true);
  };
  const hideReply = (): void => {
    if (hideTimer !== undefined) clearTimeout(hideTimer);
    hideTimer = setTimeout(() => setHovered(false), 260);
  };
  onCleanup(() => {
    if (hideTimer !== undefined) clearTimeout(hideTimer);
  });

  const openMenu = (event: MouseEvent): void => {
    event.preventDefault();
    setMenuPos({
      x: Math.min(event.clientX, window.innerWidth - 220),
      y: Math.min(event.clientY, window.innerHeight - 190),
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
        "bubble-row--group-first": firstInGroup(),
        "bubble-row--group-last": lastInGroup(),
        "bubble-row--with-avatar": !own() && props.isGroup,
      }}
      onMouseEnter={showReply}
      onMouseLeave={hideReply}
    >
      <Show when={!own() && props.isGroup}>
        <div class="bubble-row__avatar-slot">
          <Show when={lastInGroup()}>
            <Avatar info={senderAvatar()} size={34} class="bubble-row__avatar" />
          </Show>
        </div>
      </Show>

      <div
        classList={{
          bubble: true,
          "bubble--own": own(),
          "bubble--in": !own(),
          "bubble--deleted": deleted(),
          "bubble--group-first": firstInGroup(),
          "bubble--group-last": lastInGroup(),
          "bubble--active": menuPos() !== null,
        }}
        onContextMenu={openMenu}
        onDblClick={() => {
          if (!deleted()) setReplyTo(props.message);
        }}
      >
        <Show when={!deleted()}>
          <button
            classList={{
              "bubble__hover-reply": true,
              "is-visible": hovered(),
            }}
            title="Ответить"
            aria-label="Ответить"
            onMouseEnter={showReply}
            onMouseLeave={hideReply}
            onClick={() => {
              // Клик по невидимой кнопке (курсор в поле мимо пузыря) — игнор.
              if (!hovered()) return;
              setReplyTo(props.message);
            }}
          >
            <Reply size={16} />
          </button>
        </Show>
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
                color: own()
                  ? "#ffffff"
                  : senderColor(
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

        <div class="bubble__body">
          {body()}
          <span class="bubble__meta">
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
          </span>
        </div>
      </div>

      {/* Хвостик (слой ПОД пузырём: скруглённый угол «дорисовывается» хвостиком).
          Форма наша — по мотивам Telegram WebK, где угол на стороне хвостика
          прямой, а кривую даёт сам хвостик. */}
      <Show when={lastInGroup()}>
        <svg
          class="bubble__tail"
          width="11"
          height="20"
          viewBox="0 0 11 20"
          xmlns="http://www.w3.org/2000/svg"
          aria-hidden="true"
        >
          <path d="M11 0 C11 10 6 16 0 20 L11 20 Z" />
        </svg>
      </Show>

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
            <Reply size={18} />
            <span>Ответить</span>
          </button>
          <button
            role="menuitem"
            class="context-menu__item"
            onClick={copyBody}
          >
            <Copy size={18} />
            <span>Копировать текст</span>
          </button>
          <button
            role="menuitem"
            class="context-menu__item"
            disabled
            title="Пересылка появится позже"
          >
            <Forward size={18} />
            <span>Переслать</span>
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
              <Trash2 size={18} />
              <span>Удалить</span>
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
    <Switch fallback={null}>
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

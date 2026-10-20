import { Match, Show, Switch, createEffect, createSignal, onCleanup } from "solid-js";
import {
  Check,
  CheckCheck,
  CircleAlert,
  Clock,
  Copy,
  Download,
  FileText,
  Forward,
  Play,
  Reply,
  Trash2,
} from "lucide-solid";
import type { Message, MessageStatus } from "../../types/rchat";
import {
  accountState,
  deleteMessage,
  readUpToFor,
  retryMessage,
  setReplyTo,
} from "../../services/store";
import {
  attachmentThumbUrl,
  attachmentUrl,
  formatSize,
} from "../../services/attachments";
import { Avatar } from "../Avatar";
import { PrivateModeCard } from "./PrivateModeCard";
import { formatTime } from "../../utils/format";
import { messageText } from "../../utils/messages";

/** Ссылка на цитируемое сообщение (поиск по server_id в ленте чата). */
function findQuoted(profile: string, message: Message): Message | undefined {
  const quotedId = message.reply_to_server_id;
  if (quotedId === null) return undefined;
  const list = accountState(profile).messages[message.chat_id] ?? [];
  return list.find((m) => m.server_id === quotedId);
}

/** Псевдоним автора сообщения: «Вы» либо отображаемое имя участника/чата. */
function senderName(profile: string, senderId: string, chatId: string): string {
  const account = accountState(profile);
  if (account.session !== null && senderId === account.session.user_id) {
    return "Вы";
  }
  const member = account.members[chatId]?.[senderId];
  if (member !== undefined) return member.display_name || member.username;
  const chat = account.chats.find((c) => c.chat_id === chatId);
  return chat?.title ?? "Неизвестно";
}

/** Цвет имени автора: палитра ядра (avatar.color_hex). */
function senderColor(profile: string, senderId: string, chatId: string): string {
  const account = accountState(profile);
  const member = account.members[chatId]?.[senderId];
  if (member !== undefined && member.avatar.color_hex !== "") {
    return member.avatar.color_hex;
  }
  const chat = account.chats.find((c) => c.chat_id === chatId);
  return chat?.avatar.color_hex ?? "#8774e1";
}

interface MenuPos {
  x: number;
  y: number;
}

/**
 * Пузырь сообщения Telegram WebK (Amethyst Dark):
 * - Группировка последовательных сообщений одного автора (`.bubble--group-first`,
 *   `.bubble--group-last`) со скруглениями 15px / 5px (по мотивам WebK).
 * - Собственный SVG-хвостик (`11×20`) у последнего сообщения группы: угол
 *   пузыря на стороне хвостика прямой, скругление даёт сам хвостик.
 * - Фото/видео/файл/голосовое: содержимое тянется из ядра и кэшируется;
 *   у видео — постер-кадр, у голосового — звуковая волна.
 * - Двойной клик — быстрый реплай; ПКМ — контекстное меню.
 */
export function MessageBubble(props: {
  message: Message;
  showAuthor: boolean;
  isGroup: boolean;
  isFirstInGroup?: boolean;
  isLastInGroup?: boolean;
  profile: string;
  /** Переход к сообщению, на которое отвечает это (по ответу в пузыре). */
  onQuoteClick?: (serverId: number) => void;
  /** Сообщение подсвечено (только что на него перешли). */
  highlighted?: boolean;
}) {
  /** Состояние текущего аккаунта (профиля). */
  const acc = () => accountState(props.profile);
  const own = () => {
    const session = acc().session;
    return session !== null && props.message.sender_user_id === session.user_id;
  };
  const deleted = () => props.message.deleted_at_ms !== null;
  const body = () =>
    deleted() ? "Сообщение удалено" : (props.message.body ?? "");
  const quoted = () => findQuoted(props.profile, props.message);
  /** Id сообщения, на которое отвечает это (цитата). */
  const quotedId = () => props.message.reply_to_server_id;
  const firstInGroup = () => props.isFirstInGroup ?? true;
  const lastInGroup = () => props.isLastInGroup ?? true;
  const attachment = () => props.message.attachment;

  const senderAvatar = () => {
    const members = acc().members[props.message.chat_id];
    const member = members?.[props.message.sender_user_id];
    if (member !== undefined) return member.avatar;
    const chat = acc().chats.find((c) => c.chat_id === props.message.chat_id);
    return chat?.avatar;
  };

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

  // Контекстное меню (ПКМ по пузырю); закрывается кликом вне меню.
  const [menuPos, setMenuPos] = createSignal<MenuPos | null>(null);
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

  // Служебное сообщение о режиме чата — карточка по центру ленты.
  const isPrivateModeRequest = () =>
    props.message.content_kind === "private_mode_request" ||
    props.message.content_kind === "private_mode_disable";
  const privateModeEnable = () =>
    props.message.content_kind === "private_mode_request";

  return (
    <div
      classList={{
        "bubble-row": true,
        "bubble-row--own": own(),
        "bubble-row--in": !own(),
        "bubble-row--group-first": firstInGroup(),
        "bubble-row--group-last": lastInGroup(),
        "bubble-row--with-avatar": !own() && props.isGroup,
        "bubble-row--service": isPrivateModeRequest(),
        "bubble-row--highlighted": props.highlighted === true,
      }}
      data-server-id={props.message.server_id ?? undefined}
      onMouseEnter={showReply}
      onMouseLeave={hideReply}
    >
      <Show when={!own() && props.isGroup && !isPrivateModeRequest()}>
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
          "bubble--has-media": attachment() !== null && !deleted(),
          "bubble--service": isPrivateModeRequest(),
        }}
        onContextMenu={openMenu}
        onDblClick={() => {
          if (!deleted()) setReplyTo(props.profile, props.message);
        }}
      >
        {/* Хвостик — у последнего сообщения серии (как в Telegram): гладкий
            «язычок», выходящий из угла пузыря.  Для исходящих зеркалится. */}
        <Show when={lastInGroup() && !isPrivateModeRequest()}>
          <svg
            class="bubble__tail"
            width="12"
            height="14"
            viewBox="0 0 12 14"
            aria-hidden="true"
          >
            <path d="M12 0 C12 6 8 11 0 14 L12 14 Z" />
          </svg>
        </Show>

        <Show when={isPrivateModeRequest() && !deleted()}>
          <PrivateModeCard
            profile={props.profile}
            chatId={props.message.chat_id}
            requesterUserId={props.message.sender_user_id}
            enable={privateModeEnable()}
          />
        </Show>
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
              if (!hovered()) return;
              setReplyTo(props.profile, props.message);
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
                props.profile,
                props.message.sender_user_id,
                props.message.chat_id
              ),
            }}
          >
            {senderName(props.profile, props.message.sender_user_id, props.message.chat_id)}
          </span>
        </Show>

        <Show when={!deleted() && quotedId() !== null}>
          <div
            class="bubble__quote"
            role="button"
            tabIndex={0}
            title="Перейти к сообщению"
            onClick={(event) => {
              // Ответ на сообщение: переходим к нему, а не ставим свой ответ.
              event.stopPropagation();
              const id = quotedId();
              if (id !== null) props.onQuoteClick?.(id);
            }}
            onKeyDown={(event) => {
              if (event.key !== "Enter" && event.key !== " ") return;
              event.preventDefault();
              event.stopPropagation();
              const id = quotedId();
              if (id !== null) props.onQuoteClick?.(id);
            }}
          >
            <Show when={quoted() !== undefined}>
              <span
                class="bubble__quote-name"
                style={{
                  color: own()
                    ? "#ffffff"
                    : senderColor(
                        props.profile,
                        quoted()!.sender_user_id,
                        quoted()!.chat_id
                      ),
                }}
              >
                {senderName(
                  props.profile,
                  quoted()!.sender_user_id,
                  quoted()!.chat_id
                )}
              </span>
            </Show>
            <span class="bubble__quote-text">
              {quoted() !== undefined
                ? messageText(quoted()!) || "Сообщение"
                : "Сообщение"}
            </span>
          </div>
        </Show>

        <Show when={attachment() !== null && !deleted()}>
          <AttachmentView profile={props.profile} message={props.message} />
        </Show>

        <Show when={body() !== ""}>
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
                    readUpTo={readUpToFor(props.profile, props.message.chat_id)}
                  />
                </span>
              </Show>
              <Show when={props.message.status === "failed"}>
                <button
                  class="bubble__retry"
                  title="Повторить отправку"
                  onClick={(event) => {
                    event.stopPropagation();
                    void retryMessage(
                      props.profile,
                      props.message.chat_id,
                      props.message.local_id
                    );
                  }}
                >
                  Повторить
                </button>
              </Show>
            </span>
          </div>
        </Show>
        <Show when={body() === ""}>
          <div class="bubble__meta bubble__meta--alone">
            <span class="bubble__time">
              {formatTime(props.message.created_at_ms)}
            </span>
            <Show when={own() && !deleted()}>
              <span class="bubble__status">
                <StatusIcon
                  status={props.message.status}
                  serverId={props.message.server_id}
                  readUpTo={readUpToFor(props.profile, props.message.chat_id)}
                />
              </span>
            </Show>
            <Show when={props.message.status === "failed"}>
              <button
                class="bubble__retry"
                title="Повторить отправку"
                onClick={(event) => {
                  event.stopPropagation();
                  void retryMessage(
                    props.profile,
                    props.message.chat_id,
                    props.message.local_id
                  );
                }}
              >
                Повторить
              </button>
            </Show>
          </div>
        </Show>
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
              setReplyTo(props.profile, props.message);
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
                void deleteMessage(props.profile,
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

/** Альбом (медиагруппа): сетка медиа + подпись последнего элемента. */
export function AlbumBubble(props: { messages: Message[], profile: string }) {
  /** Состояние текущего аккаунта (профиля). */
  const acc = () => accountState(props.profile);
  const first = () => props.messages[0];
  const last = () => props.messages[props.messages.length - 1];
  const own = () => {
    const session = acc().session;
    return session !== null && first().sender_user_id === session.user_id;
  };
  const caption = () => last().body ?? "";

  return (
    <div
      classList={{
        "bubble-row": true,
        "bubble-row--own": own(),
        "bubble-row--in": !own(),
        "bubble-row--group-last": true,
        "bubble-row--album": true,
      }}
    >
      <Show when={!own()}>
        <div class="bubble-row__avatar-slot" />
      </Show>
      <div class="bubble bubble--album-body">
        <div
          class="bubble__album"
          style={{ "--album-items": props.messages.length }}
        >
          {props.messages.map((message) => (
            <AlbumItem profile={props.profile} message={message} />
          ))}
        </div>
        <Show when={caption() !== ""}>
          <div class="bubble__body">
            {caption()}
            <span class="bubble__meta">
              <span class="bubble__time">
                {formatTime(last().created_at_ms)}
              </span>
              <Show when={own()}>
                <span class="bubble__status">
                  <StatusIcon
                    status={last().status}
                    serverId={last().server_id}
                    readUpTo={readUpToFor(props.profile, last().chat_id)}
                  />
                </span>
              </Show>
            </span>
          </div>
        </Show>
      </div>
    </div>
  );
}

/** Один элемент альбома (медиа). */
function AlbumItem(props: { message: Message, profile: string }) {
  const [url, setUrl] = createSignal<string | null>(null);
  const [thumb, setThumb] = createSignal<string | null>(null);

  createEffect(() => {
    void attachmentUrl(props.profile, props.message).then(setUrl);
    void attachmentThumbUrl(props.profile, props.message).then(setThumb);
  });

  const kind = () => props.message.attachment?.kind ?? "file";

  return (
    <div class="bubble__album-item">
      <Switch>
        <Match when={kind() === "photo"}>
          <Show
            when={url() !== null}
            fallback={<div class="bubble__attach-skeleton">Загрузка…</div>}
          >
            <img class="bubble__image" src={url() ?? ""} alt="" />
          </Show>
        </Match>
        <Match when={kind() === "video"}>
          <Show
            when={url() !== null}
            fallback={<div class="bubble__attach-skeleton">Загрузка…</div>}
          >
            <video
              class="bubble__video"
              src={url() ?? ""}
              poster={thumb() ?? undefined}
              controls
              preload="metadata"
            />
          </Show>
        </Match>
        <Match when={true}>
          <div class="bubble__file">
            <span class="bubble__file-icon">
              <FileText size={18} />
            </span>
            <div class="bubble__file-body">
              <span class="bubble__file-name">
                {props.message.attachment?.name ?? "файл"}
              </span>
              <span class="bubble__file-size">
                {formatSize(props.message.attachment?.size ?? 0)}
              </span>
            </div>
          </div>
        </Match>
      </Switch>
    </div>
  );
}

/** Содержимое вложения: фото, видео (с постером), файл или голосовое. */
function AttachmentView(props: { message: Message, profile: string }) {
  const [url, setUrl] = createSignal<string | null>(null);
  const [thumb, setThumb] = createSignal<string | null>(null);
  const [failed, setFailed] = createSignal(false);

  createEffect(() => {
    void attachmentUrl(props.profile, props.message).then((value) => {
      if (value === null) setFailed(true);
      else setUrl(value);
    });
    void attachmentThumbUrl(props.profile, props.message).then(setThumb);
  });

  const kind = () => props.message.attachment?.kind ?? "file";

  return (
    <div class="bubble__attach">
      <Switch>
        <Match when={kind() === "photo"}>
          <Show
            when={url() !== null}
            fallback={<div class="bubble__attach-skeleton">Загрузка фото…</div>}
          >
            <img
              class="bubble__image"
              src={url() ?? ""}
              alt={props.message.attachment?.name ?? "фото"}
              onError={() => setFailed(true)}
            />
          </Show>
        </Match>
        <Match when={kind() === "video"}>
          <Show
            when={url() !== null}
            fallback={<div class="bubble__attach-skeleton">Загрузка видео…</div>}
          >
            <div class="bubble__video-wrap">
              <video
                class="bubble__video"
                src={url() ?? ""}
                poster={thumb() ?? undefined}
                controls
                preload="metadata"
              />
              <Show when={thumb() !== null}>
                <span class="bubble__video-badge">
                  <Play size={14} />
                </span>
              </Show>
            </div>
          </Show>
        </Match>
        <Match when={kind() === "voice"}>
          <Show
            when={url() !== null}
            fallback={<div class="bubble__attach-skeleton">Загрузка голосового…</div>}
          >
            <div class="bubble__voice-wrap">
              <VoiceWaveform
                waveform={props.message.attachment?.waveform ?? null}
              />
              <audio class="bubble__voice" src={url() ?? ""} controls />
            </div>
          </Show>
        </Match>
        <Match when={true}>
          <div class="bubble__file">
            <span class="bubble__file-icon">
              <FileText size={20} />
            </span>
            <div class="bubble__file-body">
              <span class="bubble__file-name">
                {props.message.attachment?.name ?? "файл"}
              </span>
              <span class="bubble__file-size">
                {formatSize(props.message.attachment?.size ?? 0)}
              </span>
            </div>
            <Show when={url() !== null}>
              <a
                class="bubble__file-download"
                href={url() ?? ""}
                download={props.message.attachment?.name ?? "file"}
                title="Скачать"
                aria-label="Скачать"
              >
                <Download size={18} />
              </a>
            </Show>
            <Show when={failed()}>
              <span class="bubble__file-size">не удалось загрузить</span>
            </Show>
          </div>
        </Match>
      </Switch>
    </div>
  );
}

/** Пики звуковой волны (base64) — рисуются полоской, как в Telegram. */
function VoiceWaveform(props: { waveform: string | null }) {
  const peaks = (): number[] => {
    if (props.waveform === null || props.waveform === "") return [];
    try {
      const binary = atob(props.waveform);
      return Array.from(binary, (char) => char.charCodeAt(0));
    } catch {
      return [];
    }
  };
  return (
    <Show when={peaks().length > 0}>
      <div class="bubble__waveform">
        {peaks().map((value) => (
          <span
            class="bubble__waveform-bar"
            style={{ height: `${Math.max(10, value)}%` }}
          />
        ))}
      </div>
    </Show>
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

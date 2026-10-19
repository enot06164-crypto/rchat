import {
  For,
  Match,
  Show,
  Switch,
  createMemo,
  createSignal,
  onMount,
} from "solid-js";
import {
  Bell,
  EllipsisVertical,
  FileText,
  Link as LinkIcon,
  Mic,
  X,
} from "lucide-solid";
import type { Chat } from "../../types/rchat";
import { ensureMembers, loadCommonGroups, state } from "../../services/store";
import { formatSize } from "../../services/attachments";
import { formatTime } from "../../utils/format";
import { Avatar } from "../Avatar";

type InfoTab = "members" | "media" | "files" | "voice" | "links" | "common";

interface TabDef {
  key: InfoTab;
  title: string;
}

/**
 * Правая колонка «Информация» (Telegram WebK `#column-right`).
 *
 * - В **личном чате** — карточка профиля собеседника: аватар, имя,
 *   `@username`, статус присутствия, тумблер уведомлений и вкладки
 *   Медиа / Файлы / Голосовые / Ссылки / Общие группы.
 * - В **группе** — те же вкладки, но вместо «Общих групп» — «Участники».
 *
 * Что умеет ядро: список чатов (`get_chats`) и состав участников
 * (`get_chat_members`) — поэтому «Общие группы» считаются честно.
 * Медиа/файлы/голосовые/ссылки ядро пока не хранит (в сообщении есть
 * только `content_kind: "text"`), поэтому эти вкладки — честные пустые
 * состояния, а не заглушки с выдуманными данными.
 */
export function InfoDrawer(props: {
  chat: Chat;
  open: boolean;
  onClose: () => void;
}) {
  const isGroup = () => props.chat.kind === "group";
  const [tab, setTab] = createSignal<InfoTab>(isGroup() ? "members" : "media");
  const [notify, setNotify] = createSignal(true);
  const [commonGroups, setCommonGroups] = createSignal<Chat[] | null>(null);

  onMount(() => {
    void ensureMembers(props.chat.chat_id);
  });

  const tabs = createMemo<TabDef[]>(() =>
    isGroup()
      ? [
          { key: "members", title: "Участники" },
          { key: "media", title: "Медиа" },
          { key: "files", title: "Файлы" },
          { key: "voice", title: "Голосовые" },
          { key: "links", title: "Ссылки" },
        ]
      : [
          { key: "media", title: "Медиа" },
          { key: "files", title: "Файлы" },
          { key: "voice", title: "Голосовые" },
          { key: "links", title: "Ссылки" },
          { key: "common", title: "Группы" },
        ]
  );

  // Собеседник личного чата: профиль из состава чата (ядро отдаёт его
  // участником даже для личной переписки).
  const peer = createMemo(() => {
    const chat = props.chat;
    if (chat.peer_user_id === null) return undefined;
    return state.members[chat.chat_id]?.[chat.peer_user_id];
  });

  const status = () => {
    const chat = props.chat;
    if (isGroup()) {
      const count = Object.keys(state.members[chat.chat_id] ?? {}).length;
      return count > 0 ? `${count} участн.` : "группа";
    }
    const online = chat.peer_user_id
      ? state.presence[chat.peer_user_id]
      : undefined;
    if (online === true) return "в сети";
    if (online === false) return "был(а) недавно";
    return "не в сети";
  };

  const openTab = (key: InfoTab): void => {
    setTab(key);
    if (key === "common" && commonGroups() === null) {
      const chat = props.chat;
      if (chat.peer_user_id === null) {
        setCommonGroups([]);
        return;
      }
      void loadCommonGroups(chat.peer_user_id).then(setCommonGroups);
    }
  };

  // Содержимое вкладок: реальные вложения и ссылки из загруженных сообщений.
  const messages = createMemo(() => state.messages[props.chat.chat_id] ?? []);

  const mediaItems = createMemo(() =>
    messages().filter(
      (message) =>
        message.attachment !== null &&
        (message.attachment.kind === "photo" ||
          message.attachment.kind === "video")
    )
  );
  const fileItems = createMemo(() =>
    messages().filter(
      (message) => message.attachment?.kind === "file"
    )
  );
  const voiceItems = createMemo(() =>
    messages().filter((message) => message.attachment?.kind === "voice")
  );
  const linkItems = createMemo(() =>
    messages()
      .map((message) => ({
        message,
        url: firstUrl(message.body ?? ""),
      }))
      .filter((item) => item.url !== null)
  );

  // Участники группы (для вкладки «Участники»).
  const memberRows = () => {
    const map = state.members[props.chat.chat_id] ?? {};
    return Object.values(map).map((member) => ({
      name: member.display_name || member.username,
      subtitle: member.username !== "" ? `@${member.username}` : "участник",
      avatar: member.avatar,
    }));
  };

  return (
    <aside
      classList={{
        "info-drawer": true,
        "is-open": props.open,
      }}
      aria-label="Информация"
      aria-hidden={!props.open}
    >
      <div class="info-drawer__inner">
        <header class="info-drawer__header">
          <button
            class="icon-button"
            title="Закрыть"
            aria-label="Закрыть"
            onClick={() => props.onClose()}
          >
            <X size={20} />
          </button>
          <span class="info-drawer__title">
            {isGroup() ? "Информация о группе" : "Информация"}
          </span>
          <button
            class="icon-button"
            title="Ещё (скоро)"
            aria-label="Ещё"
          >
            <EllipsisVertical size={20} />
          </button>
        </header>

        <div class="info-drawer__scroll">
          {/* Карточка профиля / группы */}
          <div class="info-drawer__hero">
            <Avatar info={props.chat.avatar} size={100} />
            <span class="info-drawer__name">{props.chat.title}</span>
            <Show when={!isGroup() && peer() !== undefined}>
              <span class="info-drawer__username">
                @{peer()!.username}
              </span>
            </Show>
            <span
              classList={{
                "info-drawer__status": true,
                "is-online":
                  !isGroup() &&
                  props.chat.peer_user_id !== null &&
                  state.presence[props.chat.peer_user_id] === true,
              }}
            >
              {status()}
            </span>
          </div>

          {/* Уведомления */}
          <div class="info-drawer__notify">
            <span class="info-drawer__bell">
              <Bell size={17} />
            </span>
            <span class="info-drawer__notify-label">Уведомления</span>
            <span
              classList={{ switch: true, "switch--on": notify() }}
              role="switch"
              aria-checked={notify()}
              tabIndex={0}
              onClick={() => setNotify((on) => !on)}
              onKeyDown={(event) => {
                if (event.key === "Enter" || event.key === " ") {
                  setNotify((on) => !on);
                }
              }}
            >
              <span class="switch__thumb" />
            </span>
          </div>

          {/* Вкладки содержимого */}
          <nav class="info-drawer__tabs" aria-label="Разделы информации">
            <For each={tabs()}>
              {(item) => (
                <button
                  classList={{ active: tab() === item.key }}
                  onClick={() => openTab(item.key)}
                >
                  {item.title}
                </button>
              )}
            </For>
          </nav>

          <Switch>
            <Match when={tab() === "members"}>
              <Show
                when={memberRows().length > 0}
                fallback={
                  <div class="info-drawer__empty">Пока никого нет</div>
                }
              >
                <For each={memberRows()}>
                  {(row) => (
                    <div class="info-row">
                      <Avatar info={row.avatar} size={40} />
                      <div class="info-row__body">
                        <span class="info-row__name">{row.name}</span>
                        <span class="info-row__subtitle">{row.subtitle}</span>
                      </div>
                    </div>
                  )}
                </For>
              </Show>
            </Match>

            <Match when={tab() === "media"}>
              <Show
                when={mediaItems().length > 0}
                fallback={
                  <EmptyState
                    title="Медиа пока нет"
                    hint="Пришлите фото или видео — они появятся здесь."
                  />
                }
              >
                <For each={mediaItems()}>
                  {(message) => (
                    <div class="info-row">
                      <Avatar info={state.session?.avatar} size={40} />
                      <div class="info-row__body">
                        <span class="info-row__name">
                          {message.attachment?.name ?? "медиа"}
                        </span>
                        <span class="info-row__subtitle">
                          {formatSize(message.attachment?.size ?? 0)}
                        </span>
                      </div>
                    </div>
                  )}
                </For>
              </Show>
            </Match>
            <Match when={tab() === "files"}>
              <Show
                when={fileItems().length > 0}
                fallback={
                  <EmptyState
                    title="Файлов пока нет"
                    hint="Прикреплённые файлы появятся здесь."
                  />
                }
              >
                <For each={fileItems()}>
                  {(message) => (
                    <div class="info-row">
                      <FileText size={20} />
                      <div class="info-row__body">
                        <span class="info-row__name">
                          {message.attachment?.name ?? "файл"}
                        </span>
                        <span class="info-row__subtitle">
                          {formatSize(message.attachment?.size ?? 0)}
                        </span>
                      </div>
                    </div>
                  )}
                </For>
              </Show>
            </Match>
            <Match when={tab() === "voice"}>
              <Show
                when={voiceItems().length > 0}
                fallback={
                  <EmptyState
                    title="Голосовых пока нет"
                    hint="Запишите голосовое кнопкой микрофона."
                  />
                }
              >
                <For each={voiceItems()}>
                  {(message) => (
                    <div class="info-row">
                      <Mic size={20} />
                      <div class="info-row__body">
                        <span class="info-row__name">Голосовое</span>
                        <span class="info-row__subtitle">
                          {formatSize(message.attachment?.size ?? 0)}
                        </span>
                      </div>
                    </div>
                  )}
                </For>
              </Show>
            </Match>
            <Match when={tab() === "links"}>
              <Show
                when={linkItems().length > 0}
                fallback={
                  <EmptyState
                    title="Ссылок пока нет"
                    hint="Ссылки из сообщений появятся здесь."
                  />
                }
              >
                <For each={linkItems()}>
                  {(item) => (
                    <div class="info-row">
                      <LinkIcon size={20} />
                      <div class="info-row__body">
                        <span class="info-row__name">{item.url}</span>
                        <span class="info-row__subtitle">
                          {formatTime(item.message.created_at_ms)}
                        </span>
                      </div>
                    </div>
                  )}
                </For>
              </Show>
            </Match>

            <Match when={tab() === "common"}>
              <Show
                when={commonGroups() !== null}
                fallback={<div class="info-drawer__empty">Считаем общие группы…</div>}
              >
                <Show
                  when={(commonGroups() ?? []).length > 0}
                  fallback={
                    <div class="info-drawer__empty">
                      Общих групп нет
                      <div class="info-drawer__empty-hint">
                        Считаем по составу участников групп из ядра.
                      </div>
                    </div>
                  }
                >
                  <For each={commonGroups() ?? []}>
                    {(group) => (
                      <div class="info-row">
                        <Avatar info={group.avatar} size={40} />
                        <div class="info-row__body">
                          <span class="info-row__name">{group.title}</span>
                          <span class="info-row__subtitle">общая группа</span>
                        </div>
                      </div>
                    )}
                  </For>
                </Show>
              </Show>
            </Match>
          </Switch>
        </div>
      </div>
    </aside>
  );
}

/** Первая ссылка в тексте сообщения (если есть). */
function firstUrl(text: string): string | null {
  const match = text.match(/https?:\/\/\S+/);
  return match === null ? null : match[0];
}

/** Честное пустое состояние: заголовок + пояснение, почему пусто. */
function EmptyState(props: { title: string; hint: string }) {
  return (
    <div class="info-drawer__empty">
      {props.title}
      <div class="info-drawer__empty-hint">{props.hint}</div>
    </div>
  );
}

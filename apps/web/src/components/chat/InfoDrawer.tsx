import {
  For,
  Match,
  Show,
  Switch,
  createMemo,
  createSignal,
  onMount,
} from "solid-js";
import { Bell, EllipsisVertical, X } from "lucide-solid";
import type { Chat } from "../../types/rchat";
import { ensureMembers, loadCommonGroups, state } from "../../services/store";
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
              <EmptyState
                title="Медиа пока нет"
                hint="Ядро пока не хранит вложения — этап файлов."
              />
            </Match>
            <Match when={tab() === "files"}>
              <EmptyState
                title="Файлов пока нет"
                hint="Ядро пока не хранит вложения — этап файлов."
              />
            </Match>
            <Match when={tab() === "voice"}>
              <EmptyState
                title="Голосовых пока нет"
                hint="Запись голосовых появится вместе с этапом файлов."
              />
            </Match>
            <Match when={tab() === "links"}>
              <EmptyState
                title="Ссылок пока нет"
                hint="Ядро пока не извлекает ссылки из сообщений."
              />
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

/** Честное пустое состояние: заголовок + пояснение, почему пусто. */
function EmptyState(props: { title: string; hint: string }) {
  return (
    <div class="info-drawer__empty">
      {props.title}
      <div class="info-drawer__empty-hint">{props.hint}</div>
    </div>
  );
}

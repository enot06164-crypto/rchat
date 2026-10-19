import { For, Match, Show, Switch, createSignal, onMount } from "solid-js";
import { X } from "lucide-solid";
import type { Chat } from "../../types/rchat";
import { ensureMembers, state } from "../../services/store";
import { Avatar } from "../Avatar";

type InfoTab = "members" | "media" | "files" | "links";

const TABS: { key: InfoTab; title: string }[] = [
  { key: "members", title: "Участники" },
  { key: "media", title: "Медиа" },
  { key: "files", title: "Файлы" },
  { key: "links", title: "Ссылки" },
];

/**
 * Правая шторка «Информация» (WebK): выезжает справа ~360px по клику на
 * шапку чата. Большой аватар, имя, статус, локальный тумблер уведомлений
 * и вкладки с фиолетовым индикатором. Медиа/файлы/ссылки — честные
 * пустые состояния (отправка вложений появится позже).
 */
export function InfoDrawer(props: { chat: Chat; onClose: () => void }) {
  const [tab, setTab] = createSignal<InfoTab>("members");
  const [notify, setNotify] = createSignal(true);

  onMount(() => {
    if (props.chat.kind === "group") {
      void ensureMembers(props.chat.chat_id);
    }
  });

  const subtitle = () => {
    const chat = props.chat;
    if (chat.kind === "group") {
      const count = Object.keys(state.members[chat.chat_id] ?? {}).length;
      return count > 0 ? `${count} участн.` : "группа";
    }
    const online = chat.peer_user_id
      ? state.presence[chat.peer_user_id]
      : undefined;
    if (online === true) return "в сети";
    if (online === false) return "был(а) недавно";
    return chat.e2e_mode === "olm_v1" ? "E2E защита" : "обычный чат";
  };

  // Состав списка участников: для групп — из ядра; для прямых — два имени.
  const memberRows = (): { name: string; subtitle: string; avatar: Chat["avatar"] | undefined }[] => {
    const chat = props.chat;
    if (chat.kind === "group") {
      const map = state.members[chat.chat_id] ?? {};
      return Object.values(map).map((member) => ({
        name: member.display_name || member.username,
        subtitle: member.username !== "" ? `@${member.username}` : "участник",
        avatar: member.avatar,
      }));
    }
    const rows: {
      name: string;
      subtitle: string;
      avatar: Chat["avatar"] | undefined;
    }[] = [];
    if (state.session !== null && chat.peer_user_id !== null) {
      const peer = state.members[chat.chat_id]?.[chat.peer_user_id];
      if (peer === undefined) {
        rows.push({
          name: chat.title,
          subtitle: subtitle(),
          avatar: chat.avatar,
        });
      } else {
        rows.push({
          name: peer.display_name || peer.username,
          subtitle: subtitle(),
          avatar: peer.avatar,
        });
      }
      rows.push({
        name: state.session.display_name || state.session.username,
        subtitle: "Вы",
        avatar: state.session.avatar,
      });
    } else {
      rows.push({ name: chat.title, subtitle: subtitle(), avatar: chat.avatar });
    }
    return rows;
  };

  return (
    <aside class="info-drawer" aria-label="Информация о чате">
      <header class="info-drawer__header">
        <button
          class="icon-button"
          title="Закрыть"
          aria-label="Закрыть"
          onClick={() => props.onClose()}
        >
          <X size={20} />
        </button>
        <span class="info-drawer__title">Информация о чате</span>
      </header>

      <div class="info-drawer__scroll">
        <div class="info-drawer__hero">
          <Avatar info={props.chat.avatar} size={100} />
          <span class="info-drawer__name">{props.chat.title}</span>
          <span
            classList={{
              "info-drawer__status": true,
              "is-online": subtitle() === "в сети",
            }}
          >
            {subtitle()}
          </span>

          <label class="info-drawer__notify">
            <span>Уведомления</span>
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
          </label>
        </div>

        <nav class="info-drawer__tabs" aria-label="Разделы информации">
          {TABS.map((t) => (
            <button
              classList={{ active: tab() === t.key }}
              onClick={() => setTab(t.key)}
            >
              {t.title}
            </button>
          ))}
        </nav>

        <Switch>
          <Match when={tab() === "members"}>
            <Show
              when={memberRows().length > 0}
              fallback={<div class="info-drawer__empty">Пока никого нет</div>}
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
            <div class="info-drawer__empty">Медиа пока нет</div>
          </Match>
          <Match when={tab() === "files"}>
            <div class="info-drawer__empty">Файлы пока нет</div>
          </Match>
          <Match when={tab() === "links"}>
            <div class="info-drawer__empty">Ссылки пока нет</div>
          </Match>
        </Switch>
      </div>
    </aside>
  );
}

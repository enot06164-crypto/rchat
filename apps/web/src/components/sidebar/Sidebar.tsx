import { Show, createSignal } from "solid-js";
import { LogOut, PenLine, Search } from "lucide-solid";
import { connectionLabel, logout, state } from "../../services/store";
import { Avatar } from "../Avatar";
import { ChatList } from "./ChatList";
import { NewChatModal } from "./NewChatModal";

/** Левая колонка: профиль, поиск + новый чат (карандаш), вкладки, список. */
export function Sidebar() {
  const [tab, setTab] = createSignal<"all" | "unread">("all");
  const [query, setQuery] = createSignal("");
  const [newChatOpen, setNewChatOpen] = createSignal(false);

  const linkStatus = () =>
    state.linkUp ? connectionLabel(state.connection) : "шлюз недоступен";
  const linkClass = () =>
    state.linkUp && state.connection === "connected" ? "is-online" : "";

  return (
    <aside class="sidebar">
      <header class="sidebar__header">
        <Avatar info={state.session?.avatar} size={40} />
        <div class="sidebar__me">
          <div class="sidebar__me-name">
            {state.session?.display_name ?? state.session?.username ?? ""}
          </div>
          <div class={`sidebar__me-status ${linkClass()}`}>{linkStatus()}</div>
        </div>
        <button
          class="icon-button"
          title="Выйти из аккаунта"
          aria-label="Выйти из аккаунта"
          onClick={() => void logout()}
        >
          <LogOut size={20} />
        </button>
      </header>

      <div class="sidebar__search-row">
        <div class="sidebar__search">
          <Search size={17} />
          <input
            type="text"
            placeholder="Поиск"
            value={query()}
            onInput={(event) => setQuery(event.currentTarget.value)}
          />
        </div>
        <button
          class="icon-button sidebar__add"
          title="Новый личный чат"
          aria-label="Новый личный чат"
          onClick={() => setNewChatOpen(true)}
        >
          <PenLine size={20} />
        </button>
      </div>

      <nav class="sidebar__tabs" aria-label="Папки чатов">
        <button
          classList={{ active: tab() === "all" }}
          onClick={() => setTab("all")}
        >
          Все
        </button>
        <button
          classList={{ active: tab() === "unread" }}
          onClick={() => setTab("unread")}
        >
          Непрочитанные
        </button>
      </nav>

      <ChatList tab={tab()} query={query()} />

      <Show when={newChatOpen()}>
        <NewChatModal onClose={() => setNewChatOpen(false)} />
      </Show>
    </aside>
  );
}

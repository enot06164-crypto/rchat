import { Show, createSignal, onCleanup } from "solid-js";
import { LogOut, Menu, PenLine, Search } from "lucide-solid";
import { connectionLabel, logout, state } from "../../services/store";
import { Avatar } from "../Avatar";
import { ChatList } from "./ChatList";
import { NewChatModal } from "./NewChatModal";

/**
 * Сайдбар 360px (Amethyst Dark): шапка 56px — гамбургер, овальный поиск,
 * аватар с кольцом онлайна (меню профиля); список чатов, вкладки папок
 * и плавающий фиолетовый карандаш (новый чат).
 */
export function Sidebar() {
  const [tab, setTab] = createSignal<"all" | "unread">("all");
  const [query, setQuery] = createSignal("");
  const [newChatOpen, setNewChatOpen] = createSignal(false);
  const [meMenuOpen, setMeMenuOpen] = createSignal(false);

  const linkStatus = () =>
    state.linkUp ? connectionLabel(state.connection) : "шлюз недоступен";
  const onlineNow = () => state.linkUp && state.connection === "connected";

  // Закрытие меню профиля кликом мимо него.
  const closeMenus = (event: MouseEvent): void => {
    const target = event.target as HTMLElement | null;
    if (target === null || target.closest(".me-menu, .sidebar__me-avatar")) {
      return;
    }
    setMeMenuOpen(false);
  };
  document.addEventListener("mousedown", closeMenus);
  onCleanup(() => document.removeEventListener("mousedown", closeMenus));

  return (
    <aside class="sidebar">
      <header class="sidebar__header">
        <button
          class="icon-button"
          title="Меню (папки — скоро)"
          aria-label="Меню"
        >
          <Menu size={20} />
        </button>
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
          class="sidebar__me-avatar"
          title="Профиль"
          aria-label="Профиль"
          onClick={() => setMeMenuOpen((open) => !open)}
        >
          <Avatar info={state.session?.avatar} size={34} />
          <Show when={onlineNow()}>
            <span class="sidebar__me-ring" />
          </Show>
        </button>

        <Show when={meMenuOpen()}>
          <div class="me-menu" role="menu">
            <div class="me-menu__header">
              <div class="me-menu__name">
                {state.session?.display_name ?? state.session?.username ?? ""}
              </div>
              <div class={`sidebar__me-status ${onlineNow() ? "is-online" : ""}`}>
                {linkStatus()}
              </div>
            </div>
            <button
              role="menuitem"
              class="me-menu__item"
              onClick={() => void logout()}
            >
              <LogOut size={16} />
              Выйти из аккаунта
            </button>
          </div>
        </Show>
      </header>

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

      <button
        class="fab"
        title="Новый личный чат"
        aria-label="Новый личный чат"
        onClick={() => setNewChatOpen(true)}
      >
        <PenLine size={24} />
      </button>

      <Show when={newChatOpen()}>
        <NewChatModal onClose={() => setNewChatOpen(false)} />
      </Show>
    </aside>
  );
}

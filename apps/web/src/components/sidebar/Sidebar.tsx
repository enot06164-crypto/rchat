import { For, Show, createSignal, onCleanup } from "solid-js";
import {
  ArrowLeft,
  Cloud,
  Lock,
  LogOut,
  Menu,
  PenLine,
  Plus,
  Search,
  UserPlus,
  X,
} from "lucide-solid";
import {
  accountState,
  connectionLabel,
  logout,
  searchMessages,
  setAddAccountOpen,
  setDefaultChatMode,
  state,
  switchProfile,
} from "../../services/store";
import { Avatar } from "../Avatar";
import { ChatList } from "./ChatList";
import { NewChatModal } from "./NewChatModal";
import { SearchResults } from "./SearchResults";

/**
 * Левая колонка (Telegram WebK `#column-left`):
 * - Шапка 56px: слева кнопка-гамбургер (открывает меню аккаунта WebK или
 *   сбрасывает поиск стрелкой назад), справа на всю ширину — овальное поле
 *   поиска `.input-search` с фиолетовой обводкой при фокусе.
 * - Горизонтальные вкладки папок чатов с плавным нижним индикатором.
 * - Список чатов и плавающая круглая кнопка `.fab` (`.btn-corner`) внизу справа.
 */
export function Sidebar(props: { profile: string }) {
  /** Состояние текущего аккаунта (профиля). */
  const acc = () => accountState(props.profile);
  const [query, setQuery] = createSignal("");
  const [searchFocused, setSearchFocused] = createSignal(false);
  const [newChatOpen, setNewChatOpen] = createSignal(false);
  const [meMenuOpen, setMeMenuOpen] = createSignal(false);
  const [settingsOpen, setSettingsOpen] = createSignal(false);

  const linkStatus = () =>
    state.linkUp ? connectionLabel(state.connection) : "шлюз недоступен";
  const onlineNow = () => state.linkUp && state.connection === "connected";
  const isSearching = () => searchFocused() || query().trim() !== "";

  // Закрытие главного меню кликом мимо него.
  const closeMenus = (event: MouseEvent): void => {
    const target = event.target as HTMLElement | null;
    if (target === null || target.closest(".me-menu, .sidebar__burger")) {
      return;
    }
    setMeMenuOpen(false);
  };
  document.addEventListener("mousedown", closeMenus);
  onCleanup(() => document.removeEventListener("mousedown", closeMenus));

  // Поиск по сообщениям: с задержкой, чтобы не дёргать сервер на каждую
  // букву (как в WebK).  Пустая строка — очищаем выдачу.
  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  const clearSearch = (): void => {
    if (searchTimer !== undefined) clearTimeout(searchTimer);
    setQuery("");
    void searchMessages(props.profile, "");
  };
  const onSearchInput = (value: string): void => {
    setQuery(value);
    if (searchTimer !== undefined) clearTimeout(searchTimer);
    if (value.trim() === "") {
      void searchMessages(props.profile, "");
      return;
    }
    searchTimer = setTimeout(() => void searchMessages(props.profile, value), 250);
  };
  onCleanup(() => {
    if (searchTimer !== undefined) clearTimeout(searchTimer);
  });

  const onBurgerClick = (): void => {
    if (query().trim() !== "") {
      clearSearch();
      setSearchFocused(false);
      return;
    }
    setMeMenuOpen((open) => !open);
  };

  return (
    <aside class="sidebar">
      <header class="sidebar__header">
        <button
          classList={{
            "icon-button": true,
            "sidebar__burger": true,
            "is-active": meMenuOpen(),
          }}
          title={query().trim() !== "" ? "Сбросить поиск" : "Меню и профиль"}
          aria-label={query().trim() !== "" ? "Сбросить поиск" : "Меню"}
          onClick={onBurgerClick}
        >
          <Show when={query().trim() !== ""} fallback={<Menu size={22} />}>
            <ArrowLeft size={22} />
          </Show>
        </button>

        <div
          classList={{
            "sidebar__search": true,
            "is-focused": isSearching(),
          }}
        >
          <Search size={18} class="sidebar__search-icon" />
          <input
            type="text"
            placeholder="Поиск"
            value={query()}
            onFocus={() => setSearchFocused(true)}
            onBlur={() => setSearchFocused(false)}
            onInput={(event) => onSearchInput(event.currentTarget.value)}
          />
          <Show when={query() !== ""}>
            <button
              class="sidebar__search-clear"
              title="Очистить"
              aria-label="Очистить поиск"
              onClick={clearSearch}
            >
              <X size={16} />
            </button>
          </Show>
        </div>

        <Show when={meMenuOpen()}>
          <div class="me-menu" role="menu">
            <div class="me-menu__account">
              <div class="me-menu__avatar-wrap">
                <Avatar info={acc().session?.avatar} size={38} />
                <Show when={onlineNow()}>
                  <span class="me-menu__online-ring" />
                </Show>
              </div>
              <div class="me-menu__info">
                <div class="me-menu__name">
                  {acc().session?.display_name || acc().session?.username || "Профиль"}
                </div>
                <div class={`me-menu__status ${onlineNow() ? "is-online" : ""}`}>
                  {acc().session?.username
                    ? `@${acc().session?.username} · `
                    : ""}
                  {linkStatus()}
                </div>
              </div>
            </div>

            <div class="me-menu__divider" />

            {/* Аккаунты этого браузера: как в меню Telegram Web. */}
            <div class="me-menu__accounts">
            <For each={state.profiles}>
              {(profile) => (
                <button
                  role="menuitem"
                  classList={{
                    "me-menu__item": true,
                    "me-menu__account-item": true,
                    "is-active": profile === state.activeProfile,
                  }}
                  onClick={() => {
                    setMeMenuOpen(false);
                    void switchProfile(profile);
                  }}
                >
                  <Avatar
                    info={state.accounts[profile]?.session?.avatar}
                    size={26}
                  />
                  <span class="me-menu__account-name">
                    {state.accounts[profile]?.session?.display_name ??
                      state.accounts[profile]?.session?.username ??
                      profile}
                  </span>
                </button>
              )}
            </For>

            </div>

            <button
              role="menuitem"
              class="me-menu__item me-menu__account-item"
              onClick={() => {
                setMeMenuOpen(false);
                setAddAccountOpen(true);
              }}
            >
              <span class="me-menu__plus">
                <Plus size={16} />
              </span>
              <span class="me-menu__account-name">Добавить аккаунт</span>
            </button>

            <div class="me-menu__divider" />

            <button
              role="menuitem"
              class="me-menu__item"
              onClick={() => {
                setMeMenuOpen(false);
                setNewChatOpen(true);
              }}
            >
              <UserPlus size={18} />
              <span>Новый личный чат</span>
            </button>

            <button
              role="menuitem"
              class="me-menu__item"
              onClick={() => {
                setMeMenuOpen(false);
                setSettingsOpen(true);
              }}
            >
              <Show
                when={acc().defaultChatMode === "cloud"}
                fallback={<Lock size={18} />}
              >
                <Cloud size={18} />
              </Show>
              <span>Настройки</span>
            </button>

            <button
              role="menuitem"
              class="me-menu__item me-menu__item--danger"
              onClick={() => void logout(props.profile, )}
            >
              <LogOut size={18} />
              <span>Выйти из аккаунта</span>
            </button>
          </div>
        </Show>
      </header>

      <SearchResults profile={props.profile} query={query()} />
      <ChatList profile={props.profile} query={query()} />

      <button
        classList={{ fab: true, "is-open": newChatOpen() }}
        title="Новый личный чат"
        aria-label="Новый личный чат"
        onClick={() => setNewChatOpen(true)}
      >
        <PenLine size={22} />
      </button>

      <Show when={newChatOpen()}>
        <NewChatModal profile={props.profile} onClose={() => setNewChatOpen(false)} />
      </Show>

      <Show when={settingsOpen()}>
        <div
          class="modal-backdrop"
          onClick={(event) => {
            if (event.target === event.currentTarget) setSettingsOpen(false);
          }}
        >
          <div class="modal" role="dialog" aria-modal="true">
            <div class="modal__title">Настройки</div>

            <div class="settings__row">
              <div class="settings__text">
                <div class="settings__name">Режим чатов</div>
                <div class="settings__hint">
                  {acc().defaultChatMode === "cloud"
                    ? "Облачный: сервер видит содержимое, умеет искать по истории и отдаёт её новому устройству — как обычные чаты Telegram."
                    : "Приватный: сквозное шифрование, сервер не видит содержимого."}
                </div>
              </div>
              <div class="settings__switch">
                <button
                  classList={{
                    switch: true,
                    "switch--on": acc().defaultChatMode === "cloud",
                  }}
                  role="switch"
                  aria-checked={acc().defaultChatMode === "cloud"}
                  title={
                    acc().defaultChatMode === "cloud"
                      ? "Облачный режим"
                      : "Приватный режим (E2E)"
                  }
                  onClick={() =>
                    void setDefaultChatMode(props.profile, 
                      acc().defaultChatMode === "cloud" ? "private" : "cloud"
                    )
                  }
                >
                  <span class="switch__thumb" />
                </button>
                <span class="settings__switch-label">
                  {acc().defaultChatMode === "cloud" ? "Облачный" : "Приватный"}
                </span>
              </div>
            </div>

            <div class="modal__actions">
              <button class="button button--accent" onClick={() => setSettingsOpen(false)}>
                Готово
              </button>
            </div>
          </div>
        </div>
      </Show>
    </aside>
  );
}

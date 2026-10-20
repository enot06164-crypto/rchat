import { onMount, Show } from "solid-js";
import { api } from "./services/api";
import {
  applyEvent,
  boot,
  loadProfiles,
  setState,
  state,
  storedProfile,
} from "./services/store";
import { AuthModal } from "./components/auth/AuthModal";
import { Sidebar } from "./components/sidebar/Sidebar";
import { ChatView } from "./components/chat/ChatView";

/**
 * Корневой компонент: splash → экран входа → главный экран
 * (левая колонка + чат).  Никаких перезагрузок страницы:
 * сессия и чаты приходят реактивно из store.
 */
export default function App() {
  onMount(() => {
    // Профиль (аккаунт) выбирается до подключения: он задаётся в адресе
    // соединения (`/ws?profile=...`), и у каждого профиля свои данные.
    const profile = storedProfile();
    setState("profile", profile);
    api.setProfile(profile);
    // При каждом успешном соединении с шлюзом (в т.ч. после реконнекта)
    // перечитываем состояние ядра.
    api.onConnect(() => {
      setState("linkUp", true);
      void boot();
      void loadProfiles();
    });
    api.onDisconnect(() => setState("linkUp", false));
    // События ядра (§7) применяются реактивно в store.
    api.onEvent((event) => applyEvent(event));
    api.connect();
  });

  return (
    <Show
      when={state.booted}
      fallback={
        <div class="splash">
          <div class="splash__spinner" />
          <div class="splash__hint">
            Подключение к шлюзу…
            <br />
            Убедитесь, что rchat-сервер и шлюз запущены
            (см. apps/web/README.md).
          </div>
        </div>
      }
    >
      <Show when={state.session !== null} fallback={<AuthModal />}>
        <div class="app-shell">
          <Sidebar />
          <ChatView />
        </div>
      </Show>
    </Show>
  );
}

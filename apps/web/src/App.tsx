import { onMount, Show } from "solid-js";
import { AccountPanel } from "./components/AccountPanel";
import { AddAccountScreen } from "./components/auth/AddAccountScreen";
import { AuthModal } from "./components/auth/AuthModal";
import {
  activateProfile,
  boot,
  connectProfile,
  initialProfile,
  loadProfiles,
  setAddAccountOpen,
  state,
} from "./services/store";
import { connectErrorText } from "./services/errors";

/**
 * Корневой компонент: splash → панель активного аккаунта.
 *
 * Аккаунтов может быть несколько: у каждого своё соединение с ядром (свой
 * профиль шлюза, своя БД и ключи) и своё состояние.  Все аккаунты
 * подключены одновременно, переключение — мгновенное, из меню аккаунта
 * (бургер), как в Telegram Web.
 */
export default function App() {
  onMount(() => {
    // Подключаем аккаунты, известные шлюзу, и аккаунт из адреса.
    void (async () => {
      await loadProfiles();
      const initial = initialProfile();
      for (const profile of state.profiles) connectProfile(profile);
      if (!state.profiles.includes(initial)) connectProfile(initial);
      activateProfile(initial);
      await boot(initial);
    })();
  });

  const active = () => state.accounts[state.activeProfile];

  return (
    <Show
      when={active()?.booted === true}
      fallback={
        <div class="splash">
          <div class="splash__spinner" />
          <div class="splash__hint">
            <Show
              when={state.linkError !== null}
              fallback={
                <>
                  Подключение к шлюзу…
                  <br />
                  Убедитесь, что rchat-сервер и шлюз запущены
                  (см. apps/web/README.md).
                </>
              }
            >
              Шлюз отказал в подключении
              <br />
              {state.linkError !== null ? connectErrorText(state.linkError) : ""}
            </Show>
          </div>
        </div>
      }
    >
      <div class="app-shell">
        <AccountPanel profile={state.activeProfile} />
      </div>

      <Show when={active()?.session === null}>
        <AuthModal profile={state.activeProfile} />
      </Show>

      {/* Экран «Добавить аккаунт» — поверх всего, как в Telegram Web. */}
      <Show when={state.addAccountOpen}>
        <AddAccountScreen onClose={() => setAddAccountOpen(false)} />
      </Show>
    </Show>
  );
}

import { For, Show, createSignal } from "solid-js";
import {
  login,
  nextProfileName,
  register,
  state,
  switchProfile,
} from "../../services/store";
import { describeError } from "../../services/api";

type Mode = "login" | "register";

/** Экран входа/регистрации (единственный экран при отсутствии сессии). */
export function AuthModal() {
  const [mode, setMode] = createSignal<Mode>("login");
  const [username, setUsername] = createSignal("");
  const [displayName, setDisplayName] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [localError, setLocalError] = createSignal<string | null>(null);

  const error = () => localError() ?? state.authError;

  // Аккаунты (профили шлюза): переключение — мгновенное, состояние
  // каждого аккаунта своё.
  const pickProfile = (name: string): void => {
    void switchProfile(name);
  };

  const addProfile = (): void => {
    pickProfile(nextProfileName(state.profiles));
  };

  const submit = async (): Promise<void> => {
    const name = username().trim();
    const secret = password();
    if (name === "" || secret === "") {
      setLocalError("введите username и пароль");
      return;
    }
    setLocalError(null);
    try {
      if (mode() === "login") {
        await login(name, secret);
      } else {
        await register(name, displayName().trim(), secret);
      }
    } catch (caught) {
      setLocalError(describeError(caught));
    }
  };

  return (
    <div class="auth">
      <div class="auth__card">
        <div class="auth__logo">rchat</div>
        <div class="auth__subtitle">
          Локальный мессенджер со сквозным шифрованием
        </div>

        <div class="auth__tabs" role="tablist" aria-label="Режим входа">
          <button
            classList={{ active: mode() === "login" }}
            onClick={() => {
              setMode("login");
              setLocalError(null);
            }}
          >
            Вход
          </button>
          <button
            classList={{ active: mode() === "register" }}
            onClick={() => {
              setMode("register");
              setLocalError(null);
            }}
          >
            Регистрация
          </button>
        </div>

        <Show when={state.profiles.length > 0}>
          <div class="auth__accounts">
            <div class="auth__accounts-row">
              <For each={state.profiles}>
                {(profile) => (
                  <button
                    type="button"
                    classList={{
                      "auth__account": true,
                      "is-active": profile === state.profile,
                    }}
                    title={profile}
                    onClick={() => pickProfile(profile)}
                  >
                    {profile}
                  </button>
                )}
              </For>
              <button
                type="button"
                class="auth__account auth__account--add"
                title="Добавить аккаунт"
                onClick={addProfile}
              >
                +
              </button>
            </div>
          </div>
        </Show>

        <form
          onSubmit={(event) => {
            event.preventDefault();
            void submit();
          }}
        >
          <input
            class="auth__field"
            type="text"
            placeholder="username"
            autocomplete="username"
            spellcheck={false}
            value={username()}
            onInput={(event) => {
              setUsername(event.currentTarget.value);
              setLocalError(null);
            }}
          />
          <Show when={mode() === "register"}>
            <input
              class="auth__field"
              type="text"
              placeholder="Отображаемое имя (необязательно)"
              autocomplete="name"
              value={displayName()}
              onInput={(event) => {
                setDisplayName(event.currentTarget.value);
                setLocalError(null);
              }}
            />
          </Show>
          <input
            class="auth__field"
            type="password"
            placeholder="пароль"
            autocomplete={mode() === "login" ? "current-password" : "new-password"}
            value={password()}
            onInput={(event) => {
              setPassword(event.currentTarget.value);
              setLocalError(null);
            }}
          />

          <Show when={error() !== null}>
            <div class="auth__error">{error()}</div>
          </Show>

          <button
            type="submit"
            class="button button--accent button--full"
            disabled={state.busy}
          >
            {state.busy
              ? "Подключаемся…"
              : mode() === "login"
                ? "Войти"
                : "Создать аккаунт"}
          </button>
        </form>

        <div class="auth__gateway">
          Соединение с ядром: WebSocket-шлюз {window.location.host}
        </div>
      </div>
    </div>
  );
}

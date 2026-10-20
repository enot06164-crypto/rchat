import { For, Show, createSignal } from "solid-js";
import { login, register, state, switchProfile } from "../../services/store";
import { describeError } from "../../services/api";

type Mode = "login" | "register";

/** Экран входа/регистрации (единственный экран при отсутствии сессии). */
export function AuthModal() {
  const [mode, setMode] = createSignal<Mode>("login");
  const [username, setUsername] = createSignal("");
  const [displayName, setDisplayName] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [localError, setLocalError] = createSignal<string | null>(null);
  const [newProfile, setNewProfile] = createSignal("");

  const error = () => localError() ?? state.authError;

  // Профиль (аккаунт) шлюза: у каждого свои чаты и сессия.
  const chooseProfile = (name: string): void => {
    void switchProfile(name);
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

        <div class="auth__profiles">
          <div class="auth__profiles-title">Аккаунт (профиль)</div>
          <div class="auth__profiles-list">
            <For each={state.profiles}>
              {(profile) => (
                <button
                  type="button"
                  classList={{
                    "auth__profile": true,
                    "is-active": profile === state.profile,
                  }}
                  onClick={() => chooseProfile(profile)}
                >
                  {profile}
                </button>
              )}
            </For>
          </div>
          <div class="auth__profiles-new">
            <input
              class="auth__field"
              type="text"
              placeholder="новый профиль (имя)"
              spellcheck={false}
              value={newProfile()}
              onInput={(event) => setNewProfile(event.currentTarget.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  const name = newProfile().trim();
                  if (name !== "") {
                    setNewProfile("");
                    chooseProfile(name);
                  }
                }
              }}
            />
            <button
              type="button"
              class="button button--ghost"
              disabled={newProfile().trim() === ""}
              onClick={() => {
                const name = newProfile().trim();
                if (name === "") return;
                setNewProfile("");
                chooseProfile(name);
              }}
            >
              Открыть
            </button>
          </div>
        </div>

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

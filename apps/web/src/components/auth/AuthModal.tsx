import { Show, createSignal } from "solid-js";
import {
  accountState,
  login,
  register,
  waitConnected,
} from "../../services/store";
import { describeError } from "../../services/api";

type Mode = "login" | "register";

/** Экран входа/регистрации (единственный экран при отсутствии сессии). */
export function AuthModal(props: { profile: string }) {
  /** Состояние текущего аккаунта (профиля). */
  const acc = () => accountState(props.profile);
  const [mode, setMode] = createSignal<Mode>("login");
  const [username, setUsername] = createSignal("");
  const [displayName, setDisplayName] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [localError, setLocalError] = createSignal<string | null>(null);

  const error = () => localError() ?? acc().authError;

  const submit = async (): Promise<void> => {
    const name = username().trim();
    const secret = password();
    if (name === "" || secret === "") {
      setLocalError("введите username и пароль");
      return;
    }
    setLocalError(null);
    try {
      // Пока ядро не соединилось с сервером, вход отклоняется.
      if (!(await waitConnected(props.profile))) {
        setLocalError("не удалось подключиться к серверу");
        return;
      }
      if (mode() === "login") {
        await login(props.profile, name, secret);
      } else {
        await register(props.profile, name, displayName().trim(), secret);
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
          Мессенджер со сквозным шифрованием
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
            disabled={acc().busy}
          >
            {acc().busy
              ? "Подключаемся…"
              : mode() === "login"
                ? "Войти"
                : "Создать аккаунт"}
          </button>
        </form>
      </div>
    </div>
  );
}

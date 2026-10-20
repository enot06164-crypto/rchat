import { Show, createSignal } from "solid-js";
import {
  accountState,
  login,
  register,
  waitConnected,
} from "../../services/store";
import { Notice } from "../common/Notice";
import { classifyError, type ErrorNotice } from "../../services/errors";

type Mode = "login" | "register";

/** Экран входа/регистрации (единственный экран при отсутствии сессии). */
export function AuthModal(props: { profile: string }) {
  /** Состояние текущего аккаунта (профиля). */
  const acc = () => accountState(props.profile);
  const [mode, setMode] = createSignal<Mode>("login");
  const [username, setUsername] = createSignal("");
  const [displayName, setDisplayName] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [localError, setLocalError] = createSignal<ErrorNotice | null>(null);

  const error = () => localError();

  const submit = async (): Promise<void> => {
    const name = username().trim();
    const secret = password();
    if (name === "" || secret === "") {
      setLocalError({
        kind: "invalid_input",
        title: "Заполните поля",
        message: "Введите username и пароль.",
        retryable: false,
        operation: "login",
      });
      return;
    }
    setLocalError(null);
    try {
      // Пока ядро не соединилось с сервером, вход отклоняется.
      if (!(await waitConnected(props.profile))) {
        setLocalError({
          kind: "server_offline",
          title: "Нет связи с сервером",
          message: "Ядро не подключилось. Повторите, когда соединение появится.",
          action: { kind: "wait", label: "Подождать" },
          retryable: true,
          operation: "login",
        });
        return;
      }
      if (mode() === "login") {
        await login(props.profile, name, secret);
      } else {
        await register(props.profile, name, displayName().trim(), secret);
      }
    } catch (caught) {
      setLocalError(classifyError(caught, { operation: "login" }));
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
            <Notice notice={error()!} />
          </Show>
          <Show when={acc().authError !== null}>
            <Notice
              notice={acc().authError!}
              onAction={() => {
                // «Повторить» — ещё одна попытка входа; «Войти» — форма и так
                // перед пользователем.
                void submit();
              }}
              onDismiss={() => setLocalError(null)}
            />
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

import { Show, createSignal } from "solid-js";
import { addAccount, setAddAccountOpen } from "../../services/store";

/**
 * Экран «Добавить аккаунт» — отдельная страница, как в Telegram Web:
 * вход в существующий аккаунт или регистрация нового.  Аккаунт появляется в
 * списке меню профиля только после успешного входа.
 */
export function AddAccountScreen(props: { onClose: () => void }) {
  const [mode, setMode] = createSignal<"login" | "register">("login");
  const [username, setUsername] = createSignal("");
  const [displayName, setDisplayName] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);

  const close = (): void => {
    setAddAccountOpen(false);
    props.onClose();
  };

  const submit = async (): Promise<void> => {
    const name = username().trim();
    const secret = password();
    if (name === "" || secret === "") {
      setError("введите username и пароль");
      return;
    }
    setError(null);
    setBusy(true);
    try {
      const failure = await addAccount(
        name,
        secret,
        displayName().trim(),
        mode() === "register"
      );
      if (failure !== null) {
        setError(failure);
        return;
      }
      close();
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="auth">
      <div class="auth__card">
        <div class="auth__logo">rchat</div>
        <div class="auth__subtitle">
          <button class="auth__back" onClick={close}>
            ← Назад
          </button>
          Добавить аккаунт
        </div>

        <div class="auth__tabs" role="tablist" aria-label="Режим входа">
          <button
            classList={{ active: mode() === "login" }}
            onClick={() => {
              setMode("login");
              setError(null);
            }}
          >
            Вход
          </button>
          <button
            classList={{ active: mode() === "register" }}
            onClick={() => {
              setMode("register");
              setError(null);
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
              setError(null);
            }}
          />
          <Show when={mode() === "register"}>
            <input
              class="auth__field"
              type="text"
              placeholder="Отображаемое имя (необязательно)"
              autocomplete="name"
              value={displayName()}
              onInput={(event) => setDisplayName(event.currentTarget.value)}
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
              setError(null);
            }}
          />

          <Show when={error() !== null}>
            <div class="auth__error">{error()}</div>
          </Show>

          <button
            type="submit"
            class="button button--accent button--full"
            disabled={busy()}
          >
            {busy()
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

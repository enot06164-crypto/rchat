import { Show, createSignal } from "solid-js";
import { addAccount, setAddAccountOpen } from "../../services/store";

/**
 * Карточка «Добавить аккаунт» (как в Telegram Web): вход в существующий
 * аккаунт или регистрация нового.  Аккаунт появляется в списке меню
 * профиля и становится активным.
 */
export function AddAccountModal(props: { onClose: () => void }) {
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
    <div
      class="modal-backdrop"
      onClick={(event) => {
        if (event.target === event.currentTarget) close();
      }}
    >
      <div class="modal" role="dialog" aria-modal="true">
        <div class="modal__title">Добавить аккаунт</div>
        <div class="modal__hint">
          Новый аккаунт живёт в этом браузере: свои чаты, контакты и ключи.
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
          onKeyDown={(event) => {
            if (event.key === "Enter") void submit();
          }}
        />

        <Show when={error() !== null}>
          <div class="auth__error">{error()}</div>
        </Show>

        <div class="modal__actions">
          <button class="button button--ghost" onClick={close}>
            Отмена
          </button>
          <button
            class="button button--accent"
            disabled={busy()}
            onClick={() => void submit()}
          >
            {busy()
              ? "Подключаем…"
              : mode() === "login"
                ? "Войти"
                : "Создать аккаунт"}
          </button>
        </div>
      </div>
    </div>
  );
}

import { Show, createSignal } from "solid-js";
import { createDirectChat, openChat, state } from "../../services/store";
import { describeError } from "../../services/api";

/** Диалог создания нового личного чата по username собеседника. */
export function NewChatModal(props: { onClose: () => void }) {
  const [username, setUsername] = createSignal("");
  const [localError, setLocalError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  // Приватный чат — по сквозному шифрованию; по умолчанию режим клиента
  // (облачный, как в обычных чатах Telegram).
  const [privateChat, setPrivateChat] = createSignal(state.defaultChatMode === "private");

  const error = () => localError() ?? state.newChatError;

  const submit = async (): Promise<void> => {
    setLocalError(null);
    setBusy(true);
    try {
      const chat = await createDirectChat(
        username(),
        privateChat() ? "private" : "cloud"
      );
      if (chat !== null) {
        props.onClose();
        await openChat(chat.chat_id);
      }
    } catch (caught) {
      setLocalError(describeError(caught));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      class="modal-backdrop"
      onClick={(event) => {
        if (event.target === event.currentTarget) props.onClose();
      }}
    >
      <div class="modal" role="dialog" aria-modal="true">
        <div class="modal__title">Новый чат</div>
        <input
          class="modal__input"
          placeholder="username собеседника"
          spellcheck={false}
          autocomplete="off"
          value={username()}
          onInput={(event) => {
            setUsername(event.currentTarget.value);
            setLocalError(null);
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter") void submit();
          }}
        />
        <label class="modal__checkbox">
          <input
            type="checkbox"
            checked={privateChat()}
            onChange={(event) => setPrivateChat(event.currentTarget.checked)}
          />
          <span>Приватный чат (сквозное шифрование)</span>
        </label>
        <Show when={error() !== null}>
          <div class="modal__error">{error()}</div>
        </Show>
        <div class="modal__actions">
          <button class="button button--ghost" onClick={() => props.onClose()}>
            Отмена
          </button>
          <button
            class="button button--accent"
            disabled={busy() || username().trim() === ""}
            onClick={() => void submit()}
          >
            {busy() ? "Создаём…" : "Создать"}
          </button>
        </div>
      </div>
    </div>
  );
}

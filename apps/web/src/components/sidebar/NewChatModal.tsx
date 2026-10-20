import { For, Show, createSignal, onCleanup } from "solid-js";
import { createDirectChat, openChat, searchUsers, accountState } from "../../services/store";
import { Notice } from "../common/Notice";
import { classifyError, type ErrorNotice } from "../../services/errors";
import { Avatar } from "../Avatar";
import type { UserSummary } from "../../types/rchat";

/**
 * Диалог создания нового личного чата: поиск собеседника по username
 * (как поиск контактов в Telegram) и режим чата — приватный или облачный.
 */
export function NewChatModal(props: { onClose: () => void, profile: string }) {
  /** Состояние текущего аккаунта (профиля). */
  const acc = () => accountState(props.profile);
  const [username, setUsername] = createSignal("");
  const [localError, setLocalError] = createSignal<ErrorNotice | null>(null);
  const [busy, setBusy] = createSignal(false);
  // Приватный чат — по сквозному шифрованию; по умолчанию режим клиента
  // (облачный, как в обычных чатах Telegram).
  const [privateChat, setPrivateChat] = createSignal(
    acc().defaultChatMode === "private"
  );
  const [found, setFound] = createSignal<UserSummary[]>([]);
  const [searching, setSearching] = createSignal(false);

  const error = () => localError();

  // Поиск по username с задержкой (как в WebK): не дёргаем сервер на каждую
  // букву.
  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  const onInput = (value: string): void => {
    setUsername(value);
    setLocalError(null);
    if (searchTimer !== undefined) clearTimeout(searchTimer);
    if (value.trim().length < 2) {
      setFound([]);
      setSearching(false);
      return;
    }
    setSearching(true);
    searchTimer = setTimeout(() => {
      void searchUsers(props.profile, value).then((users) => {
        setFound(users);
        setSearching(false);
      });
    }, 250);
  };
  onCleanup(() => {
    if (searchTimer !== undefined) clearTimeout(searchTimer);
  });

  const open = async (name: string): Promise<void> => {
    setLocalError(null);
    setBusy(true);
    try {
      const chat = await createDirectChat(
        props.profile,
        name,
        privateChat() ? "private" : "cloud"
      );
      if (chat !== null) {
        props.onClose();
        await openChat(props.profile, chat.chat_id);
      }
    } catch (caught) {
      setLocalError(classifyError(caught, { operation: "chat" }));
    } finally {
      setBusy(false);
    }
  };

  const submit = async (): Promise<void> => {
    const name = username().trim();
    if (name === "") {
      setLocalError({
        kind: "invalid_input",
        title: "Нужен username",
        message: "Введите имя пользователя собеседника.",
        retryable: false,
        operation: "chat",
      });
      return;
    }
    await open(name);
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
          placeholder="username или имя"
          spellcheck={false}
          autocomplete="off"
          value={username()}
          onInput={(event) => onInput(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") void submit();
          }}
        />

        {/* Найденные пользователи: выбор в один клик. */}
        <Show when={found().length > 0}>
          <div class="user-list">
            <For each={found()}>
              {(user) => (
                <button
                  class="user-list__item"
                  disabled={busy()}
                  onClick={() => void open(user.username)}
                >
                  <Avatar info={user.avatar} size={32} />
                  <span class="user-list__text">
                    <span class="user-list__name">
                      {user.display_name || user.username}
                    </span>
                    <span class="user-list__username">@{user.username}</span>
                  </span>
                </button>
              )}
            </For>
          </div>
        </Show>
        <Show when={searching() && found().length === 0}>
          <div class="user-list__hint">Ищем…</div>
        </Show>

        <label class="modal__checkbox">
          <input
            type="checkbox"
            checked={privateChat()}
            onChange={(event) => setPrivateChat(event.currentTarget.checked)}
          />
          <span>Приватный чат (сквозное шифрование)</span>
        </label>

        <Show when={error() !== null}>
          <Notice notice={error()!} />
        </Show>
        <Show when={acc().newChatError !== null}>
          <Notice
            notice={acc().newChatError!}
            onAction={() => {
              if (acc().newChatError?.action?.kind === "retry") {
                void submit();
              }
            }}
            onDismiss={() => setLocalError(null)}
          />
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

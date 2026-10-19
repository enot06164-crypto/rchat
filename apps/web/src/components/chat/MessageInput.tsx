import { Show, createSignal } from "solid-js";
import { Mic, Paperclip, SendHorizontal, Smile, X } from "lucide-solid";
import { clearReply, sendCurrentText, state } from "../../services/store";

/**
 * Строка ввода в стиле Telegram WebK: порядок кнопок как в оригинале —
 * скрепка слева, текстовое поле по центру, смайлик справа от поля и
 * круглая кнопка отправки (40px) последней.
 * Иконки микрофона и самолётика живут в DOM одновременно и плавно
 * сменяют друг друга (`grow-icon` из WebK).
 * Над строкой — блок ответа (`.reply-wrapper`): иконка, автор, текст, отмена.
 */
export function MessageInput(props: { chatId: string }) {
  const [value, setValue] = createSignal("");
  let textarea!: HTMLTextAreaElement;

  const canSend = () => value().trim() !== "";
  const reply = () =>
    state.replyTo !== null && state.replyTo.chat_id === props.chatId
      ? state.replyTo
      : null;

  const autosize = (): void => {
    textarea.style.height = "auto";
    textarea.style.height = `${Math.min(textarea.scrollHeight, 160)}px`;
  };

  const submit = (): void => {
    if (!canSend()) return;
    const text = value();
    setValue("");
    textarea.style.height = "auto";
    void sendCurrentText(text);
    textarea.focus();
  };

  return (
    <div class="composer-wrap">
      <Show when={state.sendError !== null && state.activeChatId === props.chatId}>
        <div class="composer-error">{state.sendError}</div>
      </Show>

      <div class="composer">
        <Show when={reply() !== null}>
          <div class="composer__reply">
            <span class="composer__reply-icon">
              <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
                <path
                  d="M6.2 2.4 1.6 7l4.6 4.6V8.5h3.2c1.9 0 3.1 1 3.4 2.9.5-3.3-1.3-5.6-4.4-5.6H6.2V2.4Z"
                  fill="currentColor"
                />
              </svg>
            </span>
            <div class="composer__reply-body">
              <span class="composer__reply-name">
                {reply() !== null &&
                  replyTextOf(reply()!.sender_user_id, reply()!.chat_id)}
              </span>
              <span class="composer__reply-text">
                {reply()!.body ?? "Сообщение"}
              </span>
            </div>
            <button
              class="composer__reply-cancel"
              title="Отменить ответ"
              aria-label="Отменить ответ"
              onClick={() => clearReply()}
            >
              <X size={18} />
            </button>
          </div>
        </Show>

        <div class="composer__row">
          <button
            class="composer__icon composer__icon--attach"
            title="Прикрепить файл (скоро)"
            aria-label="Прикрепить файл"
          >
            <Paperclip size={20} />
          </button>

          <textarea
            ref={textarea}
            rows={1}
            placeholder="Сообщение"
            value={value()}
            onInput={(event) => {
              setValue(event.currentTarget.value);
              autosize();
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !event.shiftKey) {
                event.preventDefault();
                submit();
              }
            }}
          />

          <button
            class="composer__icon"
            title="Эмодзи (скоро)"
            aria-label="Эмодзи"
          >
            <Smile size={22} />
          </button>

          <button
            classList={{
              "composer__send": true,
              "is-ready": canSend(),
            }}
            disabled={!canSend()}
            aria-label={canSend() ? "Отправить сообщение" : "Голосовое сообщение"}
            title={canSend() ? "Отправить (Enter)" : "Голосовые сообщения появятся позже"}
            onClick={submit}
          >
            <span class="composer__send-icon composer__send-icon--mic">
              <Mic size={20} />
            </span>
            <span class="composer__send-icon composer__send-icon--send">
              <SendHorizontal size={20} />
            </span>
          </button>
        </div>
      </div>
    </div>
  );
}

function replyTextOf(senderId: string, chatId: string): string {
  if (state.session !== null && senderId === state.session.user_id) {
    return "Вы";
  }
  const member = state.members[chatId]?.[senderId];
  if (member !== undefined) return member.display_name || member.username;
  const chat = state.chats.find((c) => c.chat_id === chatId);
  return chat?.title ?? "Неизвестно";
}

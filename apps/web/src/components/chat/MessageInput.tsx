import { Show, createSignal } from "solid-js";
import { Paperclip, SendHorizontal, Smile } from "lucide-solid";
import { sendCurrentText, state } from "../../services/store";

/**
 * «Парящая капсула» ввода в стиле Telegram WebK: смайл и скрепка слева,
 * рамочный textarea по центру, круглая синяя кнопка отправки справа.
 * Отступ снизу 16px и скругление 16px задаёт `.composer` в global.scss.
 */
export function MessageInput(props: { chatId: string }) {
  const [value, setValue] = createSignal("");
  let textarea!: HTMLTextAreaElement;

  const canSend = () => value().trim() !== "";

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
        <button
          class="composer-icon"
          title="Эмодзи (скоро)"
          aria-label="Эмодзи"
        >
          <Smile size={22} />
        </button>
        <button
          class="composer-icon"
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
          class="send-button"
          aria-label="Отправить сообщение"
          title="Отправить (Enter)"
          disabled={!canSend()}
          onClick={submit}
        >
          <SendHorizontal size={19} />
        </button>
      </div>
    </div>
  );
}

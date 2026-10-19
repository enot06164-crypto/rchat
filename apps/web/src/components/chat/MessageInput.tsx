import { Show, createSignal } from "solid-js";
import { SendHorizontal } from "lucide-solid";
import { sendCurrentText, state } from "../../services/store";

/** Поле ввода: Enter — отправка, Shift+Enter — перенос строки, самолётик. */
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
    <div class="message-input">
      <Show when={state.sendError !== null && state.activeChatId === props.chatId}>
        <div class="message-input__error">{state.sendError}</div>
      </Show>
      <div class="message-input__row">
        <textarea
          ref={textarea}
          rows={1}
          placeholder="Сообщение…"
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

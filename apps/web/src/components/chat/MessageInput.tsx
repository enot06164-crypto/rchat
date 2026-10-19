import { Show, createSignal } from "solid-js";
import { Mic, Paperclip, SendHorizontal, Smile, X } from "lucide-solid";
import { clearReply, sendCurrentText, state } from "../../services/store";

/**
 * «Парящая капсула» ввода (Amethyst Dark): скрепка и смайл слева, textarea
 * без рамки, справа — микрофон на пустом поле и фиолетовый самолётик,
 * когда есть текст. Над полем — строка реплая (реальный `reply_to_server_id`).
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
            <span class="composer__reply-bar" />
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
              class="composer-icon"
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
            class="composer-icon"
            title="Прикрепить файл (скоро)"
            aria-label="Прикрепить файл"
          >
            <Paperclip size={20} />
          </button>
          <button
            class="composer-icon"
            title="Эмодзи (скоро)"
            aria-label="Эмодзи"
          >
            <Smile size={22} />
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
          <Show
            when={canSend()}
            fallback={
              <button
                class="send-button"
                disabled
                aria-label="Голосовое сообщение"
                title="Голосовые сообщения появятся позже"
              >
                <Mic size={20} />
              </button>
            }
          >
            <button
              class="send-button"
              aria-label="Отправить сообщение"
              title="Отправить (Enter)"
              onClick={submit}
            >
              <SendHorizontal size={19} />
            </button>
          </Show>
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

import { Show, createSignal } from "solid-js";
import { Mic, Paperclip, SendHorizontal, Smile, X } from "lucide-solid";
import type { AttachmentUpload } from "../../types/rchat";
import { clearReply, sendCurrentText, state } from "../../services/store";
import { formatSize, prepareAttachment } from "../../services/attachments";

/** Лимит вложения (как в ядре: MAX_ATTACHMENT_BYTES = 128 КиБ). */
const MAX_ATTACHMENT_BYTES = 128 * 1024;

/**
 * Строка ввода в стиле Telegram WebK: скрепка слева, текстовое поле по
 * центру, смайлик справа, круглая кнопка отправки последней.
 *
 * Поддержано: отправка файла/фото/видео (кнопка скрепки) и запись
 * голосового сообщения (кнопка микрофона, MediaRecorder). Вложение едет
 * внутри зашифрованного payload, поэтому сервер не видит его содержимое.
 */
export function MessageInput(props: { chatId: string }) {
  const [value, setValue] = createSignal("");
  const [pending, setPending] = createSignal<AttachmentUpload | null>(null);
  const [recording, setRecording] = createSignal(false);
  const [attachError, setAttachError] = createSignal<string | null>(null);
  let textarea!: HTMLTextAreaElement;
  let fileInput!: HTMLInputElement;
  let recorder: MediaRecorder | null = null;
  let recorderChunks: Blob[] = [];

  const canSend = () => value().trim() !== "" || pending() !== null;
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
    const attachment = pending();
    setValue("");
    setPending(null);
    textarea.style.height = "auto";
    void sendCurrentText(text, attachment);
    textarea.focus();
  };

  const acceptFile = async (file: File): Promise<void> => {
    setAttachError(null);
    if (file.size > MAX_ATTACHMENT_BYTES) {
      setAttachError(
        `Файл больше ${formatSize(MAX_ATTACHMENT_BYTES)} — отправка вложений пока ограничена ядром.`
      );
      return;
    }
    try {
      const attachment = await prepareAttachment(file);
      // Оптимистичное сообщение умеет показать вложение сразу: содержимое
      // уже на руках, кладём его в кэш по ключу будущего local_id.
      setPending(attachment);
    } catch {
      setAttachError("Не удалось прочитать файл.");
    }
  };

  const onFileChange = (event: Event): void => {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    if (file !== undefined) void acceptFile(file);
    input.value = "";
  };

  // --- запись голосового ---
  const startRecording = async (): Promise<void> => {
    setAttachError(null);
    if (recorder !== null) return;
    if (!navigator.mediaDevices?.getUserMedia) {
      setAttachError("Браузер не умеет записывать звук.");
      return;
    }
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      recorderChunks = [];
      recorder = new MediaRecorder(stream);
      recorder.ondataavailable = (event) => {
        if (event.data.size > 0) recorderChunks.push(event.data);
      };
      recorder.onstop = () => {
        stream.getTracks().forEach((track) => track.stop());
        const blob = new Blob(recorderChunks, { type: "audio/webm" });
        recorder = null;
        setRecording(false);
        if (blob.size === 0) return;
        const file = new File([blob], "voice.webm", { type: "audio/webm" });
        void acceptFile(file);
      };
      recorder.start();
      setRecording(true);
    } catch {
      setAttachError("Доступ к микрофону запрещён.");
      setRecording(false);
    }
  };

  const stopRecording = (): void => {
    if (recorder !== null && recorder.state !== "inactive") recorder.stop();
  };

  return (
    <div class="composer-wrap">
      <Show when={state.sendError !== null && state.activeChatId === props.chatId}>
        <div class="composer-error">{state.sendError}</div>
      </Show>
      <Show when={attachError() !== null}>
        <div class="composer-error">{attachError()}</div>
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

        <Show when={pending() !== null}>
          <div class="composer__attach">
            <span class="composer__attach-name">{pending()!.name}</span>
            <span class="composer__attach-size">
              {formatSize(pending()!.size)}
            </span>
            <button
              class="composer__reply-cancel"
              title="Убрать вложение"
              aria-label="Убрать вложение"
              onClick={() => setPending(null)}
            >
              <X size={18} />
            </button>
          </div>
        </Show>

        <div class="composer__row">
          <button
            class="composer__icon composer__icon--attach"
            title="Прикрепить файл"
            aria-label="Прикрепить файл"
            onClick={() => fileInput.click()}
          >
            <Paperclip size={20} />
          </button>
          <input
            ref={fileInput}
            type="file"
            class="composer__file"
            onChange={onFileChange}
          />

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

          <Show
            when={!recording()}
            fallback={
              <button
                class="composer__send composer__send--recording"
                title="Остановить запись"
                aria-label="Остановить запись"
                onClick={stopRecording}
              >
                <span class="composer__send-icon composer__send-icon--mic" />
                <span class="composer__recording-dot" />
              </button>
            }
          >
            <button
              classList={{
                "composer__send": true,
                "is-ready": canSend(),
              }}
              disabled={!canSend()}
              aria-label={
                canSend() ? "Отправить сообщение" : "Записать голосовое"
              }
              title={canSend() ? "Отправить (Enter)" : "Записать голосовое"}
              onClick={() => {
                if (canSend()) {
                  submit();
                  return;
                }
                void startRecording();
              }}
            >
              <span class="composer__send-icon composer__send-icon--mic">
                <Mic size={20} />
              </span>
              <span class="composer__send-icon composer__send-icon--send">
                <SendHorizontal size={20} />
              </span>
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

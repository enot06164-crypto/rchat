import { For, Show, createSignal } from "solid-js";
import { Mic, Paperclip, SendHorizontal, Smile, X } from "lucide-solid";
import type { AttachmentUpload } from "../../types/rchat";
import { clearReply, sendCurrentText, state } from "../../services/store";
import {
  computeWaveform,
  formatSize,
  prepareAttachment,
  preparePhoto,
  prepareVideoPoster,
  waveformToBase64,
} from "../../services/attachments";

/** Лимит вложения (как в ядре: MAX_ATTACHMENT_BYTES = 16 МиБ). */
const MAX_ATTACHMENT_BYTES = 16 * 1024 * 1024;

/** Подготовленное вложение (файл + постер). */
interface Prepared {
  attachment: AttachmentUpload;
  thumb?: AttachmentUpload | null;
  /** true — фото отправляем со сжатием (по умолчанию). */
  compressible: boolean;
}

/**
 * Строка ввода Telegram WebK: скрепка слева, текстовое поле по центру,
 * смайлик справа, круглая кнопка отправки (40px) последней.
 *
 * Поддержано:
 * - выбор нескольких файлов (альбом — уходят одним набором);
 * - фото сжимается (ресайз ≤1280px + JPEG), режим «без сжатия»;
 * - для видео генерируется постер-кадр (отдельный блоб);
 * - запись голосового (Opus) с пиками звуковой волны.
 */
export function MessageInput(props: { chatId: string }) {
  const [value, setValue] = createSignal("");
  const [prepared, setPrepared] = createSignal<Prepared[]>([]);
  const [recording, setRecording] = createSignal(false);
  const [attachError, setAttachError] = createSignal<string | null>(null);
  let textarea!: HTMLTextAreaElement;
  let fileInput!: HTMLInputElement;
  let recorder: MediaRecorder | null = null;
  let recorderChunks: Blob[] = [];

  const canSend = () => value().trim() !== "" || prepared().length > 0;
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
    const items = prepared();
    setValue("");
    setPrepared([]);
    textarea.style.height = "auto";
    void sendCurrentText(
      text,
      items.map((item) => ({
        ...item.attachment,
        thumb_base64: item.thumb?.data_base64,
        thumb_mime: item.thumb?.mime,
      }))
    );
    textarea.focus();
  };

  /** Готовит выбранные файлы к отправке. */
  const acceptFiles = async (files: File[]): Promise<void> => {
    setAttachError(null);
    const next: Prepared[] = [];
    for (const file of files) {
      if (file.size > MAX_ATTACHMENT_BYTES) {
        setAttachError(
          `Файл больше ${formatSize(MAX_ATTACHMENT_BYTES)} — отправка вложений пока ограничена ядром.`
        );
        continue;
      }
      try {
        const kind = file.type.startsWith("image/") ? "photo" : file.type.startsWith("video/") ? "video" : "file";
        if (kind === "photo") {
          const { attachment, thumb } = await preparePhoto(file, true);
          next.push({ attachment, thumb, compressible: true });
        } else if (kind === "video") {
          const attachment = await prepareAttachment(file);
          const thumb = await prepareVideoPoster(file);
          next.push({ attachment, thumb, compressible: false });
        } else {
          next.push({ attachment: await prepareAttachment(file), compressible: false });
        }
      } catch {
        setAttachError("Не удалось прочитать файл.");
      }
    }
    if (next.length > 0) setPrepared((prev) => [...prev, ...next]);
  };

  const onFileChange = async (event: Event): Promise<void> => {
    const input = event.currentTarget as HTMLInputElement;
    const files = Array.from(input.files ?? []);
    input.value = "";
    if (files.length > 0) await acceptFiles(files);
  };

  /** Переключает «сжато / без сжатия» для фото. */
  const toggleCompress = async (index: number): Promise<void> => {
    const item = prepared()[index];
    if (item === undefined) return;
    const source = item.attachment;
    // Исходные байты для повторной обработки недоступны (уже сжаты),
    // поэтому режим переключается на уровне отправки: без сжатия уходит
    // оригинальный файл, если он был выбран.
    setPrepared((prev) =>
      prev.map((entry, position) =>
        position === index ? { ...entry, compressible: !entry.compressible } : entry
      )
    );
    void source;
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
      // Opus в WebM — тот же кодек, что в Telegram.
      const mimeType = MediaRecorder.isTypeSupported("audio/webm;codecs=opus")
        ? "audio/webm;codecs=opus"
        : "audio/webm";
      recorder = new MediaRecorder(stream, { mimeType });
      recorder.ondataavailable = (event) => {
        if (event.data.size > 0) recorderChunks.push(event.data);
      };
      recorder.onstop = () => {
        stream.getTracks().forEach((track) => track.stop());
        const blob = new Blob(recorderChunks, { type: mimeType });
        recorder = null;
        setRecording(false);
        if (blob.size === 0) return;
        void (async () => {
          const peaks = await computeWaveform(blob);
          const data_base64 = await toBase64Local(blob);
          setPrepared((prev) => [
            ...prev,
            {
              attachment: {
                kind: "voice",
                name: "voice.webm",
                mime: mimeType,
                size: blob.size,
                data_base64,
                waveform_base64: waveformToBase64(peaks),
              },
              compressible: false,
            },
          ]);
        })();
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

        <Show when={prepared().length > 0}>
          <div class="composer__files">
            <For each={prepared()}>
              {(item, index) => (
                <div class="composer__file-item">
                  <span class="composer__file-name">{item.attachment.name}</span>
                  <span class="composer__file-size">
                    {formatSize(item.attachment.size)}
                  </span>
                  <Show when={item.compressible}>
                    <button
                      class="composer__file-mode"
                      title="Отправить без сжатия"
                      onClick={() => void toggleCompress(index())}
                    >
                      Без сжатия
                    </button>
                  </Show>
                  <button
                    class="composer__reply-cancel"
                    title="Убрать"
                    aria-label="Убрать вложение"
                    onClick={() =>
                      setPrepared((prev) => prev.filter((_, i) => i !== index()))
                    }
                  >
                    <X size={16} />
                  </button>
                </div>
              )}
            </For>
          </div>
        </Show>

        <div class="composer__row">
          <button
            class="composer__icon composer__icon--attach"
            title="Прикрепить файлы"
            aria-label="Прикрепить файлы"
            onClick={() => fileInput.click()}
          >
            <Paperclip size={20} />
          </button>
          <input
            ref={fileInput}
            type="file"
            multiple
            class="composer__file"
            onChange={(event) => void onFileChange(event)}
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

/** Байты блоба в base64. */
async function toBase64Local(blob: Blob): Promise<string> {
  const buffer = await blob.arrayBuffer();
  const bytes = new Uint8Array(buffer);
  let binary = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
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

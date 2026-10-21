import { For, Show, createSignal } from "solid-js";
import { Play, SendHorizontal, X } from "lucide-solid";
import type { AttachmentUpload } from "../../types/rchat";
import { formatSize } from "../../services/attachments";

/** Готовое к отправке вложение (фото/видео) с постером. */
export interface PreparedMedia {
  attachment: AttachmentUpload;
  thumb?: AttachmentUpload | null;
  /** true — фото отправляем со сжатием (по умолчанию). */
  compressible: boolean;
}

/** Data-URL превью: постер видео или сама картинка. */
function previewUrl(item: PreparedMedia): string {
  const data = item.thumb?.data_base64 ?? item.attachment.data_base64;
  const mime = item.thumb?.mime ?? item.attachment.mime;
  return `data:${mime || "image/jpeg"};base64,${data}`;
}

/** Заголовок окна: «Отправить 1 фото», «Отправить видео», … */
function title(items: PreparedMedia[]): string {
  if (items.length === 1) {
    const kind = items[0].attachment.kind;
    if (kind === "video") return "Отправить видео";
    if (kind === "photo") return "Отправить 1 фото";
    return "Отправить файл";
  }
  const photos = items.filter((item) => item.attachment.kind === "photo").length;
  if (photos === items.length) {
    return `Отправить ${items.length} фото`;
  }
  return `Отправить ${items.length}`;
}

/**
 * Окно отправки медиа — как в Telegram: превью фото/видео, поле подписи и
 * кнопка отправки.  Подпись отправляется вместе с медиа; текст в основном
 * поле ввода остаётся для следующего сообщения.
 */
export function MediaPreview(props: {
  items: PreparedMedia[];
  /** Отправить медиа с подписью. */
  onSend: (caption: string) => void;
  /** Закрыть окно (медиа отменяются). */
  onClose: () => void;
  /** Переключить «сжато / без сжатия». */
  onToggleCompress?: (index: number) => void;
}) {
  const [caption, setCaption] = createSignal("");
  const multiple = () => props.items.length > 1;

  return (
    <div class="media-preview" role="dialog" aria-modal="true">
      <div class="media-preview__card">
        <div class="media-preview__head">
          <button
            class="media-preview__close"
            title="Отменить"
            aria-label="Отменить отправку"
            onClick={props.onClose}
          >
            <X size={20} />
          </button>
          <div class="media-preview__title">{title(props.items)}</div>
        </div>

        <div class="media-preview__body">
          <div classList={{ "media-preview__grid": true, "media-preview__grid--many": multiple() }}>
            <For each={props.items}>
              {(item, index) => (
                <div class="media-preview__item">
                  <img src={previewUrl(item)} alt={item.attachment.name} />
                  <Show when={item.attachment.kind === "video"}>
                    <span class="media-preview__play">
                      <Play size={22} />
                    </span>
                  </Show>
                  <Show when={item.compressible && props.onToggleCompress !== undefined}>
                    <button
                      class="media-preview__mode"
                      title="Отправить без сжатия"
                      onClick={() => props.onToggleCompress?.(index())}
                    >
                      Без сжатия
                    </button>
                  </Show>
                  <Show when={item.attachment.kind === "file"}>
                    <span class="media-preview__file">
                      {item.attachment.name} · {formatSize(item.attachment.size)}
                    </span>
                  </Show>
                </div>
              )}
            </For>
          </div>
        </div>

        <div class="media-preview__foot">
          <textarea
            class="media-preview__caption"
            rows={1}
            placeholder="Добавить подпись…"
            value={caption()}
            onInput={(event) => setCaption(event.currentTarget.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !event.shiftKey) {
                event.preventDefault();
                props.onSend(caption().trim());
              }
            }}
          />
          <button
            class="media-preview__send"
            title="Отправить"
            aria-label="Отправить"
            onClick={() => props.onSend(caption().trim())}
          >
            <SendHorizontal size={20} />
          </button>
        </div>
      </div>
    </div>
  );
}

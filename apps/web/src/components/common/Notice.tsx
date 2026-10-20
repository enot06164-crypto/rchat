import { Show } from "solid-js";
import type { ErrorNotice } from "../../services/errors";

/**
 * Уведомление об ошибке в стиле приложения: заголовок, пояснение и
 * подходящее действие (повторить, войти, выбрать другой файл, подождать).
 */
export function Notice(props: {
  notice: ErrorNotice;
  /** Обработчик действия (кнопка показывается, если действие есть). */
  onAction?: () => void;
  /** Скрыть уведомление. */
  onDismiss?: () => void;
}) {
  return (
    <div class="notice" role="alert">
      <div class="notice__text">
        <span class="notice__title">{props.notice.title}</span>
        <span class="notice__message">{props.notice.message}</span>
      </div>
      <Show when={props.notice.action !== undefined && props.onAction !== undefined}>
        <button
          class="notice__action"
          onClick={() => props.onAction?.()}
        >
          {props.notice.action?.label}
        </button>
      </Show>
      <Show when={props.onDismiss !== undefined}>
        <button
          class="notice__close"
          title="Скрыть"
          aria-label="Скрыть сообщение об ошибке"
          onClick={() => props.onDismiss?.()}
        >
          ×
        </button>
      </Show>
    </div>
  );
}

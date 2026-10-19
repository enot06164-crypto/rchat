import { For, Show, createEffect, createMemo, on } from "solid-js";
import type { Message } from "../../types/rchat";
import { state } from "../../services/store";
import { MessageBubble } from "./MessageBubble";

/** Лента сообщений активного чата с автоскроллом вниз. */
export function MessageList(props: { chatId: string }) {
  const messages = createMemo<Message[]>(() => state.messages[props.chatId] ?? []);
  const empty = createMemo(() => messages().length === 0);

  let container: HTMLDivElement | undefined;

  const scrollToBottom = (): void => {
    if (container !== undefined) {
      container.scrollTo({ top: container.scrollHeight });
    }
  };

  // Новое сообщение → плавно и сразу вниз; смена чата — то же самое.
  createEffect(
    on(
      () => [props.chatId, messages().length] as const,
      () => scrollToBottom(),
      { defer: true }
    )
  );

  return (
    <div class="message-list" ref={container}>
      <Show when={!empty()} fallback={
        <div class="message-list__empty">
          Нет сообщений. Напишите первое!
        </div>
      }>
        <For each={messages()}>{(message: Message) => <MessageBubble message={message} />}</For>
      </Show>
    </div>
  );
}

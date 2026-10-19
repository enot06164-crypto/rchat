import {
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  on,
} from "solid-js";
import { ChevronDown } from "lucide-solid";
import type { Message } from "../../types/rchat";
import { state } from "../../services/store";
import { MessageBubble } from "./MessageBubble";
import { formatDateDivider } from "../../utils/format";

interface FeedItem {
  key: string;
  date: string | null;
  message: Message;
  showAuthor: boolean;
}

/** Капсула-разделитель дат (WebK): «Сегодня», «Вчера», «16 сентября». */
function DateChip(props: { label: string }) {
  return <div class="date-chip">{props.label}</div>;
}

/** Лента сообщений + разделители дат + кнопка «вниз» со счётчиком. */
export function MessageList(props: { chatId: string }) {
  const isGroup = createMemo(
    () =>
      state.chats.find((c) => c.chat_id === props.chatId)?.kind === "group"
  );

  const items = createMemo<FeedItem[]>(() => {
    const list = state.messages[props.chatId] ?? [];
    const result: FeedItem[] = [];
    let lastDay = Number.NaN;
    let prevSender: string | null = null;
    for (const message of list) {
      const day = new Date(message.created_at_ms).setHours(0, 0, 0, 0);
      let date: string | null = null;
      if (day !== lastDay) {
        date = formatDateDivider(message.created_at_ms);
        lastDay = day;
        prevSender = null;
      }
      result.push({
        key: message.local_id,
        date,
        message,
        showAuthor: isGroup() && message.sender_user_id !== prevSender,
      });
      prevSender = message.sender_user_id;
    }
    return result;
  });

  let container: HTMLDivElement | undefined;
  const [showDown, setShowDown] = createSignal(false);
  const [missed, setMissed] = createSignal(0);

  const nearBottom = (): boolean => {
    if (container === undefined) return true;
    return (
      container.scrollHeight - container.scrollTop - container.clientHeight <
      120
    );
  };

  const scrollToBottom = (smooth = false): void => {
    if (container === undefined) return;
    container.scrollTo({
      top: container.scrollHeight,
      behavior: smooth ? "smooth" : "auto",
    });
    setMissed(0);
    setShowDown(false);
  };

  const onScroll = (): void => {
    const bottom = nearBottom();
    setShowDown(!bottom);
    if (bottom) setMissed(0);
  };

  // Смена чата — мягко вниз; новые сообщения — вниз только если пользователь
  // у конца ленты, иначе копим счётчик для кнопки «вниз» (как в Telegram).
  createEffect(
    on(
      () => props.chatId,
      () => scrollToBottom(),
      { defer: true }
    )
  );
  createEffect(
    on(
      () => items().length,
      () => {
        if (nearBottom()) {
          scrollToBottom();
          setShowDown(false);
        } else {
          setMissed((m) => m + 1);
          setShowDown(true);
        }
      },
      { defer: true }
    )
  );

  return (
    <div class="message-list" ref={container} onScroll={onScroll}>
      <Show
        when={items().length > 0}
        fallback={
          <div class="message-list__empty">Нет сообщений. Напишите первое!</div>
        }
      >
        <For each={items()}>
          {(item) => (
            <>
              <Show when={item.date !== null}>
                <DateChip label={item.date ?? ""} />
              </Show>
              <MessageBubble
                message={item.message}
                showAuthor={item.showAuthor}
                isGroup={isGroup()}
              />
            </>
          )}
        </For>
      </Show>

      <Show when={showDown()}>
        <button
          class="feed-down"
          aria-label="Прокрутить вниз"
          title="Вниз"
          onClick={() => scrollToBottom(true)}
        >
          <ChevronDown size={22} />
          <Show when={missed() > 0}>
            <span class="feed-down__badge">{missed()}</span>
          </Show>
        </button>
      </Show>
    </div>
  );
}

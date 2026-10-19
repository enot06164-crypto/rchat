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
  isFirstInGroup: boolean;
  isLastInGroup: boolean;
}

/** Капсула-разделитель дат (WebK `.is-date`): «Сегодня», «Вчера», «16 сентября». */
function DateChip(props: { label: string }) {
  return (
    <div class="date-chip-row">
      <div class="date-chip">{props.label}</div>
    </div>
  );
}

/** Лента сообщений + группировка пузырей WebK + разделители дат + кнопка «вниз». */
export function MessageList(props: { chatId: string }) {
  const isGroup = createMemo(
    () =>
      state.chats.find((c) => c.chat_id === props.chatId)?.kind === "group"
  );

  const items = createMemo<FeedItem[]>(() => {
    // Удалённые сообщения выпадают из ленты полностью — никаких «призрачных»
    // пузырей (как в Telegram при «удалить у себя»).
    const list = (state.messages[props.chatId] ?? []).filter(
      (message) => message.deleted_at_ms === null
    );
    const result: FeedItem[] = [];
    let lastDay = Number.NaN;
    let prevSender: string | null = null;

    for (let i = 0; i < list.length; i++) {
      const message = list[i];
      const day = new Date(message.created_at_ms).setHours(0, 0, 0, 0);
      let date: string | null = null;
      if (day !== lastDay) {
        date = formatDateDivider(message.created_at_ms);
        lastDay = day;
        prevSender = null;
      }

      const next = list[i + 1];
      const nextDay =
        next !== undefined
          ? new Date(next.created_at_ms).setHours(0, 0, 0, 0)
          : Number.NaN;
      const sameNextSender =
        next !== undefined &&
        nextDay === day &&
        next.sender_user_id === message.sender_user_id;

      const isFirstInGroup = message.sender_user_id !== prevSender;
      const isLastInGroup = !sameNextSender;

      result.push({
        key: message.local_id,
        date,
        message,
        showAuthor: isGroup() && isFirstInGroup,
        isFirstInGroup,
        isLastInGroup,
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

  // Смена чата — мгновенно вниз; новые сообщения — вниз только если пользователь
  // у конца ленты, иначе копим счётчик для кнопки «вниз» (как в Telegram WebK).
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
      <div class="message-list__inner">
        <Show
          when={items().length > 0}
          fallback={
            <div class="message-list__empty">
              <div class="message-list__empty-card">
                <div class="message-list__empty-title">Здесь пока пусто</div>
                <div class="message-list__empty-subtitle">
                  Отправьте первое сообщение — переписка защищена сквозным шифрованием.
                </div>
              </div>
            </div>
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
                  isFirstInGroup={item.isFirstInGroup}
                  isLastInGroup={item.isLastInGroup}
                />
              </>
            )}
          </For>
        </Show>
      </div>

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

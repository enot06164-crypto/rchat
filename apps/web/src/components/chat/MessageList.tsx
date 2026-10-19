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
import { AlbumBubble, MessageBubble } from "./MessageBubble";
import { formatDateDivider } from "../../utils/format";

/** Единица ленты: обычное сообщение или альбом (медиагруппа). */
type FeedUnit =
  | { kind: "message"; key: string; date: string | null; message: Message; showAuthor: boolean; isFirstInGroup: boolean; isLastInGroup: boolean }
  | { kind: "album"; key: string; date: string | null; messages: Message[] };

/** Капсула-разделитель дат (WebK `.is-date`): «Сегодня», «Вчера», «16 сентября». */
function DateChip(props: { label: string }) {
  return (
    <div class="date-chip-row">
      <div class="date-chip">{props.label}</div>
    </div>
  );
}

/** Лента сообщений + группировка пузырей WebK + альбомы + кнопка «вниз». */
export function MessageList(props: { chatId: string }) {
  const isGroup = createMemo(
    () =>
      state.chats.find((c) => c.chat_id === props.chatId)?.kind === "group"
  );

  const units = createMemo<FeedUnit[]>(() => {
    // Удалённые сообщения выпадают из ленты полностью — никаких «призрачных»
    // пузырей (как в Telegram при «удалить у себя»).
    const list = (state.messages[props.chatId] ?? []).filter(
      (message) => message.deleted_at_ms === null
    );
    const result: FeedUnit[] = [];
    let lastDay = Number.NaN;
    let prevSender: string | null = null;
    let index = 0;

    while (index < list.length) {
      const message = list[index];
      const day = new Date(message.created_at_ms).setHours(0, 0, 0, 0);
      let date: string | null = null;
      if (day !== lastDay) {
        date = formatDateDivider(message.created_at_ms);
        lastDay = day;
        prevSender = null;
      }

      // Альбом: подряд идущие сообщения с общим grouped_id.
      const groupedId = message.grouped_id ?? null;
      if (groupedId !== null) {
        const album: Message[] = [];
        while (index < list.length && list[index].grouped_id === groupedId) {
          album.push(list[index]);
          index += 1;
        }
        result.push({
          kind: "album",
          key: `a-${groupedId}`,
          date,
          messages: album,
        });
        prevSender = album[album.length - 1].sender_user_id;
        continue;
      }

      const next = list[index + 1];
      const nextDay =
        next !== undefined
          ? new Date(next.created_at_ms).setHours(0, 0, 0, 0)
          : Number.NaN;
      const sameNextSender =
        next !== undefined &&
        nextDay === day &&
        next.sender_user_id === message.sender_user_id &&
        (next.grouped_id ?? null) === null;

      result.push({
        kind: "message",
        key: message.local_id,
        date,
        message,
        showAuthor: isGroup() && message.sender_user_id !== prevSender,
        isFirstInGroup: message.sender_user_id !== prevSender,
        isLastInGroup: !sameNextSender,
      });
      prevSender = message.sender_user_id;
      index += 1;
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
      () => units().length,
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
          when={units().length > 0}
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
          <For each={units()}>
            {(unit) => (
              <>
                <Show when={unit.date !== null}>
                  <DateChip label={unit.date ?? ""} />
                </Show>
                <Show
                  when={unit.kind === "album"}
                  fallback={
                    <MessageBubble
                      message={unit.kind === "message" ? unit.message : unit.messages[0]}
                      showAuthor={unit.kind === "message" ? unit.showAuthor : false}
                      isGroup={isGroup()}
                      isFirstInGroup={
                        unit.kind === "message" ? unit.isFirstInGroup : true
                      }
                      isLastInGroup={
                        unit.kind === "message" ? unit.isLastInGroup : true
                      }
                    />
                  }
                >
                  <AlbumBubble
                    messages={unit.kind === "album" ? unit.messages : []}
                  />
                </Show>
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

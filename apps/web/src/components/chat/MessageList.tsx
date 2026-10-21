import {
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  on,
  onCleanup,
  onMount,
} from "solid-js";
import { ChevronDown } from "lucide-solid";
import type { Message } from "../../types/rchat";
import {
  accountState,
  ensureMessageLoaded,
  noticeHistoryFailure,
  noticeMessageNotFound,
} from "../../services/store";
import { AlbumBubble, MessageBubble } from "./MessageBubble";
import { formatDateDivider } from "../../utils/format";
import { isSameGroup } from "../../utils/messages";

/** Единица ленты: обычное сообщение или альбом (медиагруппа). */
type FeedUnit =
  | { kind: "message"; key: string; date: string | null; message: Message; showAuthor: boolean; isFirstInGroup: boolean; isLastInGroup: boolean; entering: boolean }
  | { kind: "album"; key: string; date: string | null; messages: Message[]; entering: boolean };

/** Капсула-разделитель дат (WebK `.is-date`): «Сегодня», «Вчера», «16 сентября». */
function DateChip(props: { label: string }) {
  return (
    <div class="date-chip-row">
      <div class="date-chip">{props.label}</div>
    </div>
  );
}

/** Лента сообщений + группировка пузырей WebK + альбомы + кнопка «вниз». */
export function MessageList(props: { chatId: string, profile: string }) {
  /** Состояние текущего аккаунта (профиля). */
  const acc = () => accountState(props.profile);
  const isGroup = createMemo(
    () =>
      acc().chats.find((c) => c.chat_id === props.chatId)?.kind === "group"
  );

  // Ключи уже показанных сообщений (см. ниже).
  const shownKeys = new Set<string>();

  const units = createMemo<FeedUnit[]>(() => {
    // Удалённые сообщения выпадают из ленты полностью — никаких «призрачных»
    // пузырей (как в Telegram при «удалить у себя»).
    const list = (acc().messages[props.chatId] ?? []).filter(
      (message) => message.deleted_at_ms === null
    );
    const result: FeedUnit[] = [];
    // Ключи уже показанных сообщений: по ним помечаем новое сообщение, чтобы
    // анимация появления играла только у него (иначе при любом обновлении
    // ленты все пузыри мигали заново).
    const shown = shownKeys;
    let lastDay = Number.NaN;
    let prevSender: string | null = null;
    // Продолжение ли это серии (предыдущее сообщение — тот же автор, тот же
    // день, пауза меньше GROUP_GAP_MS).
    let prevInGroup = false;
    let index = 0;

    while (index < list.length) {
      const message = list[index];
      const day = new Date(message.created_at_ms).setHours(0, 0, 0, 0);
      let date: string | null = null;
      if (day !== lastDay) {
        date = formatDateDivider(message.created_at_ms);
        lastDay = day;
        prevSender = null;
        prevInGroup = false;
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
          entering: false,
        });
        prevSender = album[album.length - 1].sender_user_id;
        prevInGroup = false;
        continue;
      }

      const next = list[index + 1];
      // Серия: тот же автор, тот же день и пауза меньше пяти минут.  В
      // Telegram большая пауза разрывает серию — у каждого пузыря свой
      // хвостик.
      const sameNextSender =
        next !== undefined && isSameGroup(message, next);

      result.push({
        kind: "message",
        key: message.local_id,
        date,
        message,
        showAuthor: isGroup() && message.sender_user_id !== prevSender,
        isFirstInGroup: !prevInGroup,
        isLastInGroup: !sameNextSender,
        entering: !shown.has(message.local_id),
      });
      prevSender = sameNextSender ? message.sender_user_id : null;
      prevInGroup = sameNextSender;
      index += 1;
    }
    // Запоминаем показанные ключи: в следующий раз это сообщение уже не
    // «новое».
    shown.clear();
    for (const unit of result) shown.add(unit.key);
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

  // «Липкий низ»: пока пользователь не ушёл вверх, новое сообщение
  // прокручивает ленту вниз.  Проверять «близко ли к низу» в момент
  // прихода сообщения нельзя — лента как раз выросла, и расстояние до низа
  // увеличилось.  Поэтому держим флаг и меняем его только когда прокрутил
  // сам пользователь.
  const [stick, setStick] = createSignal(true);
  let lastScrollTop = 0;
  let lastScrollHeight = 0;

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
    if (container === undefined) return;
    const top = container.scrollTop;
    const height = container.scrollHeight;
    // Лента выросла сама (пришло сообщение): позиция не изменилась — это
    // не действие пользователя, «липкость» сохраняется.
    const contentGrew = height > lastScrollHeight && top === lastScrollTop;
    if (!contentGrew) {
      setStick(nearBottom());
    }
    lastScrollTop = top;
    lastScrollHeight = height;
    const bottom = nearBottom();
    setShowDown(!bottom);
    if (bottom) setMissed(0);
  };

  // Лента может подросить, когда догрузилось фото или видео (размер медиа
  // известен только после метаданных).  Если пользователь внизу — доезжаем
  // вместе с ней, иначе последний пузырь упирается в поле ввода.
  onMount(() => {
    if (container === undefined) return;
    const inner = container.querySelector(".message-list__inner");
    if (inner === null) return;
    const observer = new ResizeObserver(() => {
      if (stick()) scrollToBottom();
    });
    observer.observe(inner);
    onCleanup(() => observer.disconnect());
  });

  // Переход к сообщению, на которое отвечает пузырь: прокручиваем к нему и
  // ненадолго подсвечиваем (как в Telegram).
  const [highlightedId, setHighlightedId] = createSignal<number | null>(null);
  let highlightTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => {
    if (highlightTimer !== undefined) clearTimeout(highlightTimer);
  });

  const findRow = (serverId: number): HTMLElement | null => {
    if (container === undefined) return null;
    return container.querySelector<HTMLElement>(
      `[data-server-id="${serverId}"]`
    );
  };

  const scrollToMessage = async (serverId: number): Promise<void> => {
    if (container === undefined) return;
    let row = findRow(serverId);
    if (row === null) {
      // Сообщения ещё нет в ленте — подгружаем более старую историю.
      try {
        const found = await ensureMessageLoaded(
          props.profile,
          props.chatId,
          serverId
        );
        if (!found) {
          noticeMessageNotFound(props.profile);
          return;
        }
      } catch (error) {
        console.warn("rchat: не удалось подгрузить историю", error);
        noticeHistoryFailure(props.profile, error, props.chatId);
        return;
      }
      // Ждём отрисовку обновлённой ленты.
      await new Promise((resolve) => setTimeout(resolve, 0));
      row = findRow(serverId);
      if (row === null) return;
    }
    row.scrollIntoView({ behavior: "smooth", block: "center" });
    setHighlightedId(serverId);
    if (highlightTimer !== undefined) clearTimeout(highlightTimer);
    highlightTimer = setTimeout(() => setHighlightedId(null), 1800);
  };

  // Смена чата — мгновенно вниз; новые сообщения — вниз только если пользователь
  // у конца ленты, иначе копим счётчик для кнопки «вниз» (как в Telegram WebK).
  createEffect(
    on(
      () => props.chatId,
      () => {
        setHighlightedId(null);
        // Новый чат открываем снизу, а лента при загрузке истории сама
        // доедет до последнего сообщения.
        setStick(true);
        lastScrollTop = 0;
        lastScrollHeight = 0;
        scrollToBottom();
      },
      { defer: true }
    )
  );
  createEffect(
    on(
      () => units().length,
      () => {
        if (stick()) {
          // Пользователь внизу (или ещё ни разу не прокручивал) — ведём к
          // последнему сообщению.
          scrollToBottom();
          setShowDown(false);
          setMissed(0);
        } else {
          // Читает историю — не трогаем позицию, показываем кнопку «вниз».
          setMissed((m) => m + 1);
          setShowDown(true);
        }
      },
      { defer: true }
    )
  );

  return (
    // Кнопка «вниз» — рядом с лентой, а не внутри неё: внутри она
    // прокручивается вместе с сообщениями и уезжает от поля ввода.
    <div class="message-list-wrap">
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
                      profile={props.profile}
                      message={unit.kind === "message" ? unit.message : unit.messages[0]}
                      showAuthor={unit.kind === "message" ? unit.showAuthor : false}
                      isGroup={isGroup()}
                      isFirstInGroup={
                        unit.kind === "message" ? unit.isFirstInGroup : true
                      }
                      isLastInGroup={
                        unit.kind === "message" ? unit.isLastInGroup : true
                      }
                      entering={unit.entering}
                      onQuoteClick={(serverId) => {
                        void scrollToMessage(serverId);
                      }}
                      highlighted={
                        unit.kind === "message" &&
                        highlightedId() !== null &&
                        highlightedId() === unit.message.server_id
                      }
                    />
                  }
                >
                  <AlbumBubble
                    profile={props.profile}
                    messages={unit.kind === "album" ? unit.messages : []}
                  />
                </Show>
              </>
            )}
          </For>
        </Show>
      </div>
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

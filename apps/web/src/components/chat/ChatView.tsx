import { Show, createMemo } from "solid-js";
import { Phone, Search, EllipsisVertical } from "lucide-solid";
import { state } from "../../services/store";
import { Avatar } from "../Avatar";
import { MessageList } from "./MessageList";
import { MessageInput } from "./MessageInput";

/**
 * Правая колонка: шапка 56px (WebK), центральная колонна ленты и ввода
 * (max-width 720px) над фирменным фоном с радиальным градиентом и паттерном.
 *
 * Иконки поиска по чату, звонка и меню — визуальные заглушки v1
 * (см. «Ограничения» в apps/web/README.md): команды звонков появятся
 * на этапе звонков.
 */
export function ChatView() {
  const chat = createMemo(
    () => state.chats.find((c) => c.chat_id === state.activeChatId) ?? null
  );

  const subtitle = () => {
    const current = chat();
    if (current === null) return "";
    if (current.kind === "group") {
      const memberCount = Object.keys(
        state.members[current.chat_id] ?? {}
      ).length;
      const base =
        memberCount > 0 ? `группа · ${memberCount} участн.` : "группа";
      return current.e2e_mode === "olm_v1" ? `${base} · E2E защита` : base;
    }
    // Собеседник: если в этой сессии приходило presence_changed — показываем
    // статус; иначе единственный честный факт — признак шифрования.
    const online = current.peer_user_id
      ? state.presence[current.peer_user_id]
      : undefined;
    if (online === true) return "в сети";
    if (online === false) return "был(а) недавно";
    return current.e2e_mode === "olm_v1" ? "E2E защита" : "обычный чат";
  };

  return (
    <Show
      when={chat()}
      fallback={
        <div class="chat-view chat-view--empty">
          <div class="chat-view__placeholder">
            Выберите чат слева или создайте новый — карандаш рядом с поиском.
          </div>
        </div>
      }
      keyed
    >
      {(current) => (
        <section class="chat-view">
          <header class="chat-view__header">
            <Avatar info={current.avatar} size={40} />
            <div class="chat-view__title-col">
              <span class="chat-view__title">{current.title}</span>
              <span class="chat-view__subtitle">{subtitle()}</span>
            </div>
            <div class="chat-view__actions">
              <button
                class="icon-button"
                title="Поиск по чату (скоро)"
                aria-label="Поиск по чату"
              >
                <Search size={20} />
              </button>
              <button
                class="icon-button"
                title="Позвонить (скоро)"
                aria-label="Позвонить"
              >
                <Phone size={20} />
              </button>
              <button
                class="icon-button"
                title="Ещё (скоро)"
                aria-label="Ещё"
              >
                <EllipsisVertical size={20} />
              </button>
            </div>
          </header>

          <div class="chat-main">
            <MessageList chatId={current.chat_id} />
            <MessageInput chatId={current.chat_id} />
          </div>
        </section>
      )}
    </Show>
  );
}

import { Show, createMemo, createSignal } from "solid-js";
import { Phone, Search, EllipsisVertical } from "lucide-solid";
import { state } from "../../services/store";
import { Avatar } from "../Avatar";
import { MessageList } from "./MessageList";
import { MessageInput } from "./MessageInput";
import { InfoDrawer } from "./InfoDrawer";

/**
 * Центральная колонка: шапка 56px (клик открывает шторку «Информация»),
 * лента и ввод — центральный столб 720px над фоном Amethyst с паттерном.
 * Иконки поиска/звонка/меню — визуальные заглушки v1 (см. README).
 */
export function ChatView() {
  const chat = createMemo(
    () => state.chats.find((c) => c.chat_id === state.activeChatId) ?? null
  );
  const [infoOpen, setInfoOpen] = createSignal(false);

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
            Выберите чат слева или создайте новый — карандаш внизу списка.
          </div>
        </div>
      }
      keyed
    >
      {(current) => (
        <section class="chat-view">
          <header class="chat-view__header" onClick={() => setInfoOpen(true)}>
            <Avatar info={current.avatar} size={40} />
            <div class="chat-view__title-col">
              <span class="chat-view__title">{current.title}</span>
              <span class="chat-view__subtitle">{subtitle()}</span>
            </div>
            <div
              class="chat-view__actions"
              onClick={(event) => event.stopPropagation()}
            >
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

          <Show when={infoOpen()}>
            <InfoDrawer
              chat={current}
              onClose={() => setInfoOpen(false)}
            />
          </Show>
        </section>
      )}
    </Show>
  );
}

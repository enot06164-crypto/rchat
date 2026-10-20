import { Show, createMemo, createSignal } from "solid-js";
import { Phone, Search, EllipsisVertical, PanelRight } from "lucide-solid";
import { accountState } from "../../services/store";
import { Avatar } from "../Avatar";
import { MessageList } from "./MessageList";
import { MessageInput } from "./MessageInput";
import { InfoDrawer } from "./InfoDrawer";

/**
 * Центральная область (`#column-center` + `#column-right` из Telegram WebK):
 * - Шапка 56px (`.topbar`): аватар 42px, имя и статус слева; справа кнопки
 *   действий `.chat-utils` (поиск, звонок, переключатель боковой панели
 *   информации `PanelRight`, меню `EllipsisVertical`).
 * - Рабочая зона: центральный поток чата (`720px` по центру) и правая
 *   колонка «Информация» (`360px`), которая плавно сдвигает область чата,
 *   не перекрывая пузыри сообщений.
 */
export function ChatView(props: { profile: string }) {
  /** Состояние текущего аккаунта (профиля). */
  const acc = () => accountState(props.profile);
  const chat = createMemo(
    () => acc().chats.find((c) => c.chat_id === acc().activeChatId) ?? null
  );
  const [infoOpen, setInfoOpen] = createSignal(false);

  const isOnline = () => {
    const current = chat();
    if (current === null || current.kind === "group" || !current.peer_user_id) {
      return false;
    }
    return acc().presence[current.peer_user_id] === true;
  };

  const subtitle = () => {
    const current = chat();
    if (current === null) return "";
    if (current.kind === "group") {
      const memberCount = Object.keys(
        acc().members[current.chat_id] ?? {}
      ).length;
      const base =
        memberCount > 0 ? `группа · ${memberCount} участн.` : "группа";
      return current.e2e_mode === "olm_v1" ? `${base} · E2E защита` : base;
    }
    const online = current.peer_user_id
      ? acc().presence[current.peer_user_id]
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
        <section
          classList={{
            "chat-view": true,
            "is-right-column-shown": infoOpen(),
          }}
        >
          <div class="chat-view__center">
            {/* Лента, шапка и ввод — центральная колонка 720px (как в Telegram) */}
            <div class="chat-main">
              <header
                class="chat-view__header"
                onClick={() => setInfoOpen((open) => !open)}
              >
                <Avatar info={current.avatar} size={42} />
                <div class="chat-view__title-col">
                  <span class="chat-view__title">{current.title}</span>
                  <span
                    classList={{
                      "chat-view__subtitle": true,
                      "is-online": isOnline(),
                    }}
                  >
                    {subtitle()}
                  </span>
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
                    classList={{
                      "icon-button": true,
                      "is-active": infoOpen(),
                    }}
                    title="Информация о чате"
                    aria-label="Информация о чате"
                    onClick={() => setInfoOpen((open) => !open)}
                  >
                    <PanelRight size={20} />
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

              <MessageList profile={props.profile} chatId={current.chat_id} />
              <MessageInput profile={props.profile} chatId={current.chat_id} />
            </div>
          </div>

          <InfoDrawer
            profile={props.profile}
            chat={current}
            open={infoOpen()}
            onClose={() => setInfoOpen(false)}
          />
        </section>
      )}
    </Show>
  );
}

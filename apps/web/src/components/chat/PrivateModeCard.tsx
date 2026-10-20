import { Show } from "solid-js";
import { Lock, ShieldCheck, ShieldOff } from "lucide-solid";
import {
  accountState,
  cancelPrivateMode,
  resolvePrivateMode,
} from "../../services/store";

/**
 * Карточка «Запрос на приватный режим» — служебное сообщение в чате.
 *
 * Запросившему показываем ожидание подтверждения и кнопку отмены,
 * собеседнику — выбор «Принять» / «Отклонить».  Отдельный запрос бывает и
 * обратным: предложить вернуть чат в обычный режим.
 */
export function PrivateModeCard(props: {
  profile: string;
  chatId: string;
  requesterUserId: string;
  /** `true` — предложение включить приватный режим, `false` — выключить. */
  enable: boolean;
}) {
  const request = () =>
    accountState(props.profile).privateModeRequests[props.chatId];
  const me = () => accountState(props.profile).session?.user_id ?? "";
  const isRequester = () => me() === props.requesterUserId;
  const status = () => request()?.status ?? "pending";
  const pending = () => status() === "pending";
  const enable = () => props.enable;

  return (
    <div class="private-mode-card">
      <div class="private-mode-card__icon">
        <Show when={pending()} fallback={<ShieldCheck size={30} />}>
          <Show when={enable()} fallback={<ShieldOff size={30} />}>
            <Lock size={30} />
          </Show>
        </Show>
      </div>

      <Show when={pending()}>
        <div class="private-mode-card__title">
          {enable()
            ? "Запрос на приватный режим"
            : "Предложение отключить приватный режим"}
        </div>
        <div class="private-mode-card__hint">
          {enable()
            ? "Защита от копирования и скриншотов. Собеседнику уйдёт предложение включить сквозное шифрование в этом чате."
            : "Чат вернётся в обычный режим: новые сообщения сможет прочитать сервер, старая переписка останется зашифрованной."}
        </div>

        <div class="private-mode-card__status">
          <span class="private-mode-card__spinner" />
          Ожидание подтверждения от пользователя…
        </div>

        <Show
          when={isRequester()}
          fallback={
            <div class="private-mode-card__actions">
              <button
                class="button button--ghost"
                onClick={() =>
                  void resolvePrivateMode(props.profile, props.chatId, false)
                }
              >
                Отклонить
              </button>
              <button
                class="button button--accent"
                onClick={() =>
                  void resolvePrivateMode(props.profile, props.chatId, true)
                }
              >
                Принять
              </button>
            </div>
          }
        >
          <div class="private-mode-card__actions">
            <button
              class="button button--ghost"
              onClick={() => void cancelPrivateMode(props.profile, props.chatId)}
            >
              Отменить запрос
            </button>
          </div>
        </Show>
      </Show>

      <Show when={status() === "accepted"}>
        <div class="private-mode-card__title">
          {enable()
            ? "Чат переведён в приватный режим"
            : "Чат возвращён в обычный режим"}
        </div>
        <div class="private-mode-card__hint">
          {enable()
            ? "Новые сообщения защищены сквозным шифрованием."
            : "Новые сообщения видны серверу, старая переписка осталась зашифрованной."}
        </div>
      </Show>
      <Show when={status() === "declined"}>
        <div class="private-mode-card__title">Запрос отклонён</div>
        <div class="private-mode-card__hint">Режим чата не изменился.</div>
      </Show>
      <Show when={status() === "cancelled"}>
        <div class="private-mode-card__title">Запрос отозван</div>
      </Show>
    </div>
  );
}

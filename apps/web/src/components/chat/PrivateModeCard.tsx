import { Show } from "solid-js";
import { Lock, ShieldCheck } from "lucide-solid";
import {
  accountState,
  cancelPrivateMode,
  resolvePrivateMode,
} from "../../services/store";

/**
 * Карточка «Запрос на приватный режим» — обычное сообщение в чате со
 * служебным видом.  Запросившему показываем ожидание подтверждения и кнопку
 * отмены, собеседнику — выбор «Принять» / «Отклонить».
 */
export function PrivateModeCard(props: {
  profile: string;
  chatId: string;
  requesterUserId: string;
}) {
  const request = () =>
    accountState(props.profile).privateModeRequests[props.chatId];
  const me = () => accountState(props.profile).session?.user_id ?? "";
  const isRequester = () => me() === props.requesterUserId;
  const status = () => request()?.status ?? "pending";
  const pending = () => status() === "pending";

  return (
    <div class="private-mode-card">
      <div class="private-mode-card__icon">
        <Show when={pending()} fallback={<ShieldCheck size={30} />}>
          <Lock size={30} />
        </Show>
      </div>

      <Show when={pending()}>
        <div class="private-mode-card__title">
          <Show when={isRequester()} fallback="Запрос на приватный режим">
            Запрос на приватный режим
          </Show>
        </div>
        <div class="private-mode-card__hint">
          Защита от копирования и скриншотов. Собеседнику уйдёт предложение
          включить сквозное шифрование в этом чате.
        </div>

        <Show
          when={isRequester()}
          fallback={
            <div class="private-mode-card__status">
              <span class="private-mode-card__spinner" />
              Ожидание подтверждения от пользователя…
            </div>
          }
        >
          <div class="private-mode-card__status">
            <span class="private-mode-card__spinner" />
            Ожидание подтверждения от пользователя…
          </div>
        </Show>

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
        <div class="private-mode-card__title">Чат переведён в приватный режим</div>
        <div class="private-mode-card__hint">
          Новые сообщения защищены сквозным шифрованием.
        </div>
      </Show>
      <Show when={status() === "declined"}>
        <div class="private-mode-card__title">Запрос отклонён</div>
        <div class="private-mode-card__hint">
          Чат остался в обычном режиме.
        </div>
      </Show>
      <Show when={status() === "cancelled"}>
        <div class="private-mode-card__title">Запрос отозван</div>
      </Show>
    </div>
  );
}

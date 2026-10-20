import { Show } from "solid-js";
import { Lock, ShieldCheck } from "lucide-solid";
import {
  accountState,
  cancelPrivateMode,
  resolvePrivateMode,
} from "../../services/store";

/**
 * Окно ожидания подтверждения: «Запрос на приватный режим».  Собеседник
 * ещё не ответил — можно отозвать запрос.
 */
export function PrivateModeWaiting(props: { profile: string; chatId: string }) {
  return (
    <div class="modal-backdrop">
      <div class="modal private-mode" role="dialog" aria-modal="true">
        <div class="private-mode__lock">
          <Lock size={34} />
        </div>
        <div class="modal__title">Запрос на приватный режим</div>
        <div class="private-mode__hint">
          Защита от копирования и скриншотов. Собеседнику уйдёт
          предложение включить сквозное шифрование в этом чате.
        </div>
        <div class="private-mode__status">
          <span class="private-mode__spinner" />
          Ожидание подтверждения от пользователя…
        </div>
        <div class="modal__actions">
          <button
            class="button button--ghost button--full"
            onClick={() => void cancelPrivateMode(props.profile, props.chatId)}
          >
            Отменить запрос
          </button>
        </div>
      </div>
    </div>
  );
}

/**
 * Входящее предложение включить приватный режим: «Собеседник предлагает
 * включить приватный режим. E2E-шифрование» — принять или отклонить.
 */
export function PrivateModeOffer(props: { profile: string }) {
  const offer = () => accountState(props.profile).privateModeOffer;
  return (
    <Show when={offer()}>
      {(current) => (
        <div class="modal-backdrop">
          <div class="modal private-mode" role="dialog" aria-modal="true">
            <div class="private-mode__shield">
              <ShieldCheck size={36} />
            </div>
            <div class="modal__title">
              {current().requester_display_name || current().requester_username} предлагает
              включить приватный режим
            </div>
            <div class="private-mode__hint">
              E2E-шифрование: содержимое чата будет защищено от копирования и
              скриншотов. Новые сообщения станут зашифрованными, старые
              останутся как есть.
            </div>
            <div class="modal__actions">
              <button
                class="button button--ghost"
                onClick={() =>
                  void resolvePrivateMode(props.profile, current().chat_id, false)
                }
              >
                Отклонить
              </button>
              <button
                class="button button--accent"
                onClick={() =>
                  void resolvePrivateMode(props.profile, current().chat_id, true)
                }
              >
                Принять
              </button>
            </div>
          </div>
        </div>
      )}
    </Show>
  );
}

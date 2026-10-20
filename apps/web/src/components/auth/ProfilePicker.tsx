import { For } from "solid-js";
import { nextProfileName, state, switchProfile } from "../../services/store";

/**
 * Выбор аккаунта (профиля шлюза): у каждого свои чаты, контакты и ключи,
 * поэтому переключение переоткрывает соединение с ядром.
 */
export function ProfilePicker(props: { onClose: () => void }) {
  const pick = (name: string): void => {
    props.onClose();
    void switchProfile(name);
  };

  return (
    <div
      class="modal-backdrop"
      onClick={(event) => {
        if (event.target === event.currentTarget) props.onClose();
      }}
    >
      <div class="modal" role="dialog" aria-modal="true">
        <div class="modal__title">Аккаунты</div>

        <div class="auth__accounts-row">
          <For each={state.profiles}>
            {(profile) => (
              <button
                type="button"
                classList={{
                  "auth__account": true,
                  "is-active": profile === state.profile,
                }}
                title={profile}
                onClick={() => pick(profile)}
              >
                {profile}
              </button>
            )}
          </For>
          <button
            type="button"
            class="auth__account auth__account--add"
            title="Добавить аккаунт"
            onClick={() => pick(nextProfileName(state.profiles))}
          >
            +
          </button>
        </div>

        <div class="modal__actions">
          <button class="button button--accent" onClick={props.onClose}>
            Закрыть
          </button>
        </div>
      </div>
    </div>
  );
}

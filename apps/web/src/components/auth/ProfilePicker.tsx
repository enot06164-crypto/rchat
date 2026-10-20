import { For, Show, createSignal } from "solid-js";
import { state, switchProfile } from "../../services/store";

/**
 * Выбор профиля (аккаунта) шлюза: у каждого профиля своя БД, секреты и
 * E2E-состояние, поэтому переключение переоткрывает соединение с ядром.
 */
export function ProfilePicker(props: { onClose: () => void }) {
  const [newProfile, setNewProfile] = createSignal("");

  const open = (name: string): void => {
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

        <div class="auth__profiles-list">
          <For each={state.profiles}>
            {(profile) => (
              <button
                type="button"
                classList={{
                  "auth__profile": true,
                  "is-active": profile === state.profile,
                }}
                onClick={() => open(profile)}
              >
                {profile}
              </button>
            )}
          </For>
        </div>

        <div class="auth__profiles-new">
          <input
            class="auth__field"
            type="text"
            placeholder="новый профиль (имя)"
            spellcheck={false}
            value={newProfile()}
            onInput={(event) => setNewProfile(event.currentTarget.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                const name = newProfile().trim();
                if (name !== "") {
                  setNewProfile("");
                  open(name);
                }
              }
            }}
          />
          <button
            type="button"
            class="button button--ghost"
            disabled={newProfile().trim() === ""}
            onClick={() => {
              const name = newProfile().trim();
              if (name === "") return;
              setNewProfile("");
              open(name);
            }}
          >
            Открыть
          </button>
        </div>

        <Show when={state.profiles.length > 1}>
          <div class="modal__error" style={{ color: "var(--text-secondary, #aaa)" }}>
            У каждого профиля свои чаты, контакты и ключи — они не смешиваются.
          </div>
        </Show>

        <div class="modal__actions">
          <button class="button button--accent" onClick={props.onClose}>
            Закрыть
          </button>
        </div>
      </div>
    </div>
  );
}

import { For, Show } from "solid-js";
import { MessageCircle } from "lucide-solid";
import type { SearchHit } from "../../types/rchat";
import { openChat, state } from "../../services/store";

/**
 * Результаты поиска по сообщениям (облачный режим: сервер расшифровывает
 * содержимое ключом чата).  Показываются под списком чатов, как в WebK:
 * заголовок раздела «Сообщения» и список найденных фрагментов.
 */
export function SearchResults(props: { query: string }) {
  const hits = (): SearchHit[] => state.searchHits;
  const snippet = (hit: SearchHit): string => {
    const body = hit.body.replace(/\s+/g, " ").trim();
    const query = props.query.trim().toLowerCase();
    const index = body.toLowerCase().indexOf(query);
    if (index < 0) return body.length > 90 ? `${body.slice(0, 90)}…` : body;
    const start = Math.max(0, index - 30);
    const end = Math.min(body.length, index + 70);
    const prefix = start > 0 ? "…" : "";
    const suffix = end < body.length ? "…" : "";
    return `${prefix}${body.slice(start, end)}${suffix}`;
  };

  return (
    <Show when={props.query.trim() !== "" && hits().length > 0}>
      <div class="search-results">
        <div class="search-results__title">Сообщения</div>
        <For each={hits()}>
          {(hit) => (
            <button
              class="search-results__item"
              onClick={() => void openChat(hit.chat_id)}
            >
              <MessageCircle size={16} class="search-results__icon" />
              <span class="search-results__text">
                <span class="search-results__chat">
                  {hit.chat_title !== "" ? hit.chat_title : "Чат"}
                </span>
                <span class="search-results__snippet">{snippet(hit)}</span>
              </span>
            </button>
          )}
        </For>
      </div>
    </Show>
  );
}

import { Show } from "solid-js";
import type { AvatarInfo } from "../types/rchat";

/**
 * Круглый аватар с инициалами и цветом из ядра (`avatar.color_hex`).
 * Ядро само считает инициалы и детерминированный цвет (§5.6 docs/ffi.md),
 * а поверх накладывается мягкий вертикальный градиент как в Telegram WebK.
 */
export function Avatar(props: {
  info: AvatarInfo | undefined;
  size: number;
  class?: string;
}) {
  const initials = (): string => (props.info?.initials ?? "").trim();
  const fallbackColor = "#8774e1";
  return (
    <div
      class={`avatar ${props.class ?? ""}`}
      style={{
        width: `${props.size}px`,
        height: `${props.size}px`,
        "font-size": `${Math.max(11, Math.round(props.size * 0.39))}px`,
        "background-color": props.info?.color_hex ?? fallbackColor,
      }}
    >
      <Show when={initials() !== ""}>{initials()}</Show>
    </div>
  );
}

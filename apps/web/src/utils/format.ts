/** Форматирование времени в духе Telegram WebK. */

const TIME_OPTIONS: Intl.DateTimeFormatOptions = {
  hour: "2-digit",
  minute: "2-digit",
  hour12: false,
};

const timeFormatter = new Intl.DateTimeFormat("ru-RU", TIME_OPTIONS);

/** `15:42` для времени сообщения. */
export function formatTime(timestampMs: number): string {
  if (!Number.isFinite(timestampMs)) return "";
  return timeFormatter.format(new Date(timestampMs));
}

/** `15:42` для сегодняшних дат, `дд.мм` для прошлых (в списке чатов). */
export function formatShortTime(timestampMs: number): string {
  if (!Number.isFinite(timestampMs)) return "";
  const date = new Date(timestampMs);
  const now = new Date();
  const sameDay =
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate();
  if (sameDay) return timeFormatter.format(date);
  const day = String(date.getDate()).padStart(2, "0");
  const month = String(date.getMonth() + 1).padStart(2, "0");
  return `${day}.${month}`;
}

const dayFormatter = new Intl.DateTimeFormat("ru-RU", {
  day: "numeric",
  month: "long",
});

const dayYearFormatter = new Intl.DateTimeFormat("ru-RU", {
  day: "numeric",
  month: "long",
  year: "numeric",
});

/** «Сегодня» / «Вчера» / «16 сентября» (с годом, если он не текущий). */
export function formatDateDivider(timestampMs: number): string {
  if (!Number.isFinite(timestampMs)) return "";
  const date = new Date(timestampMs);
  const today = new Date();
  const yesterday = new Date();
  yesterday.setDate(today.getDate() - 1);
  const sameDay = (a: Date, b: Date): boolean =>
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate();
  if (sameDay(date, today)) return "Сегодня";
  if (sameDay(date, yesterday)) return "Вчера";
  if (date.getFullYear() === today.getFullYear()) {
    return dayFormatter.format(date);
  }
  return dayYearFormatter.format(date);
}

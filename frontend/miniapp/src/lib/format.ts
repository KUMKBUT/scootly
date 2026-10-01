// Форматирование для UI истории.
const dateTime = new Intl.DateTimeFormat('ru-RU', {
  day: 'numeric',
  month: 'short',
  hour: '2-digit',
  minute: '2-digit',
});

/** '2026-10-01T12:34:56Z' → «1 окт., 12:34» */
export function formatDateTime(iso: string): string {
  return dateTime.format(new Date(iso));
}

// Тариф per_minute (MVP #4): фикс разблокировки + цена минуты.
// Зеркало backend/services/rental-service/src/services/tariff.rs —
// клиент тикает стоимость между снапшотами GET /rides/{id}, сервер — истина.
// Деньги — только в копейках, никогда float.

export const DEFAULT_UNLOCK_KOPEKS = 2900;
export const DEFAULT_PER_MIN_KOPEKS = 800;

export interface Tariff {
  unlock_kopeks: number;
  per_min_kopeks: number;
}

export const DEFAULT_TARIFF: Tariff = {
  unlock_kopeks: DEFAULT_UNLOCK_KOPEKS,
  per_min_kopeks: DEFAULT_PER_MIN_KOPEKS,
};

/** Неполная минута считается целой, минимум — 1 минута (как на сервере). */
export function elapsedMinutes(startedAt: string, now: number = Date.now()): number {
  const startedMs = new Date(startedAt).getTime();
  const elapsedSecs = Math.max(0, Math.floor((now - startedMs) / 1000));
  return Math.max(1, Math.floor(elapsedSecs / 60) + (elapsedSecs % 60 !== 0 ? 1 : 0));
}

export function rideCostKopeks(
  startedAt: string,
  now: number = Date.now(),
  tariff: Tariff = DEFAULT_TARIFF,
): number {
  return tariff.unlock_kopeks + tariff.per_min_kopeks * elapsedMinutes(startedAt, now);
}

/** 3700 → «37,00 ₽» */
export function formatKopeks(kopeks: number): string {
  return `${(kopeks / 100).toFixed(2).replace('.', ',')} ₽`;
}

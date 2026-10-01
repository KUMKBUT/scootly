import { useEffect, useState } from 'react';
import { elapsedMinutes, rideCostKopeks } from 'shared';

export interface RideTick {
  totalMin: number;
  amountKopeks: number;
}

/**
 * Тик стоимости активной поездки (MVP #4): пересчёт каждую секунду по
 * started_at тем же алгоритмом, что на сервере (shared/utils/tariff).
 * Снапшоты GET /rides/{id} остаются истиной — тик только для UI между ними.
 */
export function useRideTick(startedAt: string | null | undefined): RideTick | null {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (!startedAt) {
      return;
    }
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [startedAt]);

  if (!startedAt) {
    return null;
  }
  return {
    totalMin: elapsedMinutes(startedAt, now),
    amountKopeks: rideCostKopeks(startedAt, now),
  };
}

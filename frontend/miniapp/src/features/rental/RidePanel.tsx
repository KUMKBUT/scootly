import { Timer } from 'lucide-react';
import { formatKopeks, type Ride } from 'shared';
import { useRideTick } from './useRideTick';

interface RidePanelProps {
  ride: Ride | null;
  finishing?: boolean;
  onFinish?: () => void;
}

/** Активная поездка: тикающий счётчик минут и стоимости, кнопка финиша. */
export function RidePanel({ ride, finishing = false, onFinish }: RidePanelProps) {
  const isActive = ride?.status === 'active';
  const tick = useRideTick(isActive ? ride.started_at : null);
  const totalMin = tick?.totalMin ?? ride?.total_min ?? null;
  const amount = tick?.amountKopeks ?? ride?.amount_kopeks ?? null;

  if (!ride) {
    return null;
  }

  return (
    <div className="rounded-2xl bg-black p-4 text-white shadow-lg">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2 text-sm text-white/70">
          <Timer size={16} />
          <span>{isActive ? 'Поездка идёт' : 'Поездка завершена'}</span>
        </div>
        <span className="text-2xl font-bold">{amount !== null ? formatKopeks(amount) : '—'}</span>
      </div>
      <div className="mt-2 flex items-center justify-between">
        <span className="text-sm text-white/70">{totalMin !== null ? `${totalMin} мин` : ''}</span>
        {isActive && (
          <button
            type="button"
            onClick={onFinish}
            disabled={finishing}
            className="rounded-xl bg-lime-400 px-4 py-2 text-sm font-semibold text-black disabled:opacity-50"
          >
            {finishing ? 'Завершаем…' : 'Завершить поездку'}
          </button>
        )}
      </div>
    </div>
  );
}

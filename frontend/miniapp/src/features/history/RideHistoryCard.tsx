import { Bike } from 'lucide-react';
import { formatKopeks, type Ride } from 'shared';
import { formatDateTime } from '../../lib/format';
import { RIDE_STATUS_LABEL } from './labels';

const STATUS_CLASS: Record<Ride['status'], string> = {
  active: 'bg-lime-400/20 text-lime-700',
  finished: 'bg-neutral-100 text-neutral-600',
  failed: 'bg-red-100 text-red-600',
};

/** Строка истории поездок: дата, статус, минуты, сумма и статус платежа. */
export function RideHistoryCard({ ride }: { ride: Ride }) {
  return (
    <div className="flex items-center gap-3 rounded-2xl bg-white p-4 shadow-sm">
      <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-full bg-neutral-100">
        <Bike size={18} className="text-neutral-500" />
      </div>
      <div className="min-w-0 flex-1">
        <div className="text-sm font-medium text-neutral-900">{formatDateTime(ride.started_at)}</div>
        <div className="text-xs text-neutral-500">
          {ride.total_min != null ? `${ride.total_min} мин` : '—'}
          {ride.payment ? ` · ${PAYMENT_SUBSTATUS[ride.payment.status] ?? ''}` : ''}
        </div>
      </div>
      <div className="text-right">
        <div className="text-sm font-semibold text-neutral-900">
          {ride.amount_kopeks != null ? formatKopeks(ride.amount_kopeks) : '—'}
        </div>
        <span className={`inline-block rounded-full px-2 py-0.5 text-xs ${STATUS_CLASS[ride.status]}`}>
          {RIDE_STATUS_LABEL[ride.status]}
        </span>
      </div>
    </div>
  );
}

const PAYMENT_SUBSTATUS: Record<string, string> = {
  hold: 'холд',
  captured: 'списано',
  canceled: 'отменён',
  refunded: 'возврат',
};

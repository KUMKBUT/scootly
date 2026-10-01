import { CreditCard } from 'lucide-react';
import { formatKopeks, type Payment } from 'shared';
import { formatDateTime } from '../../lib/format';
import { PAYMENT_STATUS_LABEL } from './labels';

/** Строка истории платежей: дата, статус, сумма. */
export function PaymentHistoryCard({ payment }: { payment: Payment }) {
  return (
    <div className="flex items-center gap-3 rounded-2xl bg-white p-4 shadow-sm">
      <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-full bg-neutral-100">
        <CreditCard size={18} className="text-neutral-500" />
      </div>
      <div className="min-w-0 flex-1">
        <div className="text-sm font-medium text-neutral-900">
          {formatDateTime(payment.created_at)}
        </div>
        <div className="text-xs text-neutral-500">{PAYMENT_STATUS_LABEL[payment.status]}</div>
      </div>
      <div className="text-sm font-semibold text-neutral-900">
        {formatKopeks(payment.amount_kopeks)}
      </div>
    </div>
  );
}

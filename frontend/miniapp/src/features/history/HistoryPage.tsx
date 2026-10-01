import { useState, type ReactNode } from 'react';
import { Bike, CreditCard } from 'lucide-react';
import { PaymentHistoryCard } from './PaymentHistoryCard';
import { RideHistoryCard } from './RideHistoryCard';
import { usePaymentHistory } from './usePaymentHistory';
import { useRideHistory } from './useRideHistory';

type Tab = 'rides' | 'payments';

const TAB_CLASS = (active: boolean) =>
  `flex flex-1 items-center justify-center gap-2 rounded-xl px-3 py-2 text-sm font-medium transition-colors ${
    active ? 'bg-black text-white' : 'text-neutral-600'
  }`;

/** История (MVP #7): вкладки «Поездки» и «Платежи», поездки — с дозагрузкой. */
export function HistoryPage() {
  const [tab, setTab] = useState<Tab>('rides');

  return (
    <div className="flex flex-col gap-4">
      <div className="flex gap-1 rounded-2xl bg-neutral-200/60 p-1">
        <button type="button" className={TAB_CLASS(tab === 'rides')} onClick={() => setTab('rides')}>
          <Bike size={16} />
          Поездки
        </button>
        <button
          type="button"
          className={TAB_CLASS(tab === 'payments')}
          onClick={() => setTab('payments')}
        >
          <CreditCard size={16} />
          Платежи
        </button>
      </div>

      {tab === 'rides' ? <RidesTab /> : <PaymentsTab />}
    </div>
  );
}

function RidesTab() {
  const rides = useRideHistory();

  if (rides.isPending) {
    return <ListPlaceholder>Загружаем поездки…</ListPlaceholder>;
  }
  if (rides.isError) {
    return (
      <ListPlaceholder>
        Не удалось загрузить поездки
        <RetryButton onClick={() => rides.refetch()} />
      </ListPlaceholder>
    );
  }

  const items = rides.data.pages.flatMap((page) => page.items);
  if (items.length === 0) {
    return <ListPlaceholder>Пока нет поездок — самое время прокатиться</ListPlaceholder>;
  }

  return (
    <div className="flex flex-col gap-2">
      {items.map((ride) => (
        <RideHistoryCard key={ride.id} ride={ride} />
      ))}
      {rides.hasNextPage && (
        <button
          type="button"
          onClick={() => rides.fetchNextPage()}
          disabled={rides.isFetchingNextPage}
          className="rounded-2xl bg-white px-4 py-3 text-sm font-medium text-neutral-700 shadow-sm disabled:opacity-50"
        >
          {rides.isFetchingNextPage ? 'Загружаем…' : 'Показать ещё'}
        </button>
      )}
    </div>
  );
}

function PaymentsTab() {
  const payments = usePaymentHistory();

  if (payments.isPending) {
    return <ListPlaceholder>Загружаем платежи…</ListPlaceholder>;
  }
  if (payments.isError) {
    return (
      <ListPlaceholder>
        Не удалось загрузить платежи
        <RetryButton onClick={() => payments.refetch()} />
      </ListPlaceholder>
    );
  }

  const items = payments.data ?? [];
  if (items.length === 0) {
    return <ListPlaceholder>Платежей пока нет</ListPlaceholder>;
  }

  return (
    <div className="flex flex-col gap-2">
      {items.map((payment) => (
        <PaymentHistoryCard key={payment.id} payment={payment} />
      ))}
    </div>
  );
}

function ListPlaceholder({ children }: { children: ReactNode }) {
  return (
    <div className="flex flex-col items-center gap-3 rounded-2xl bg-white px-4 py-10 text-center text-sm text-neutral-500 shadow-sm">
      {children}
    </div>
  );
}

function RetryButton({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="rounded-xl bg-black px-4 py-2 text-sm font-medium text-white"
    >
      Повторить
    </button>
  );
}

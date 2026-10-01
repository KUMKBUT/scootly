// История платежей (MVP #7): одна страница (openapi listPayments без курсора,
// максимум 100 — для MVP хватает).
import { useQuery } from '@tanstack/react-query';
import { historyApi } from '../../api/client';

const PAGE_SIZE = 50;

export function usePaymentHistory() {
  return useQuery({
    queryKey: ['history', 'payments'],
    queryFn: () => historyApi.listPayments({ limit: PAGE_SIZE }),
  });
}

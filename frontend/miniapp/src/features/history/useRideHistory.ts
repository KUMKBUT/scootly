// История поездок (MVP #7): TanStack Query с курсорной пагинацией —
// next_before из openapi listRides докармливает следующую страницу.
import { useInfiniteQuery } from '@tanstack/react-query';
import { historyApi } from '../../api/client';

const PAGE_SIZE = 20;

export function useRideHistory() {
  return useInfiniteQuery({
    queryKey: ['history', 'rides'],
    queryFn: ({ pageParam }) => historyApi.listRides({ limit: PAGE_SIZE, before: pageParam }),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (lastPage) => lastPage.next_before ?? undefined,
  });
}

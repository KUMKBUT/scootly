// API-клиент истории (MVP #7): GET /api/v1/rides (listRides) и
// GET /api/v1/payments (listPayments) — зеркало docs/api/openapi.yaml.
// Фабрика: конкретные baseUrl и токен приложение даёт само (miniapp/mobile).

import type { RidesPage } from '../types/ride';
import type { Payment } from '../types/payment';
import { apiRequest, type ApiConfig } from './http';

export interface ListRidesParams {
  limit?: number;
  /** Курсор пагинации — started_at из next_before предыдущей страницы. */
  before?: string;
}

export interface ListPaymentsParams {
  ride_id?: string;
  limit?: number;
}

export function createHistoryApi(config: ApiConfig) {
  return {
    /** История поездок: свежие сверху, курсорная пагинация по started_at. */
    listRides: (params: ListRidesParams = {}): Promise<RidesPage> =>
      apiRequest(config, '/api/v1/rides', params),

    /** История платежей: свежие сверху, опциональный фильтр по поездке. */
    listPayments: (params: ListPaymentsParams = {}): Promise<Payment[]> =>
      apiRequest(config, '/api/v1/payments', params),
  };
}

export type HistoryApi = ReturnType<typeof createHistoryApi>;

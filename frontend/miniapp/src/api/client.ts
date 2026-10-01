// Клиент API miniapp: baseUrl — nginx (см. openapi servers), токен — auth-стор.
import { createHistoryApi } from 'shared';
import { useAuthStore } from '../store/auth';

export const historyApi = createHistoryApi({
  baseUrl: import.meta.env.VITE_API_BASE_URL ?? 'http://localhost:9000',
  getAccessToken: () => useAuthStore.getState().accessToken,
});

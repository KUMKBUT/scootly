// Русские подписи статусов (openapi Ride.status / Payment.status).
import type { PaymentStatus, RideStatus } from 'shared';

export const RIDE_STATUS_LABEL: Record<RideStatus, string> = {
  active: 'Активна',
  finished: 'Завершена',
  failed: 'Не состоялась',
};

export const PAYMENT_STATUS_LABEL: Record<PaymentStatus, string> = {
  hold: 'Холд',
  captured: 'Списано',
  canceled: 'Отменён',
  refunded: 'Возврат',
};

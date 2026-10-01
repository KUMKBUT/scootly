// Типы поездок (MVP #4) — зеркало docs/api/openapi.yaml (`Ride`).
// miniapp и mobile импортируют отсюда, не дублируют.

export type RideStatus = 'active' | 'finished' | 'failed';

export interface Ride {
  id: string;
  scooter_id: string;
  reservation_id?: string | null;
  status: RideStatus;
  started_at: string;
  finished_at?: string | null;
  total_min?: number | null;
  /** Для active — текущая стоимость на момент ответа (копейки). */
  amount_kopeks?: number | null;
}

export interface StartRideRequest {
  scooter_id: string;
  reservation_id?: string;
}

export interface FinishRideRequest {
  lat?: number;
  lon?: number;
}

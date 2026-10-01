// Типы оплат (MVP #5) — зеркало docs/api/openapi.yaml (`Payment`, `PaymentMethod`).
// miniapp и mobile импортируют отсюда, не дублируют.

export type PaymentStatus = 'hold' | 'captured' | 'canceled' | 'refunded';

/** Платёж поездки: холд на старте → capture на финише (ADR-0003/0014). */
export interface Payment {
  id: string;
  ride_id?: string | null;
  status: PaymentStatus;
  /** Копейки: для hold — сумма холда, после capture — итог поездки. */
  amount_kopeks: number;
  created_at: string;
}

/** Привязанная карта (MVP — максимум одна). */
export interface PaymentMethod {
  id: string;
  card_last4: string;
  card_network?: string;
}

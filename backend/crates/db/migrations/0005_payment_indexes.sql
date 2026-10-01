-- MVP #5 (оплата, ADR-0003/0014): индексы payments.
-- Таблица payments из 0001 уже содержит UNIQUE idempotency_key
-- (hold:{rental_id} / ride:{rental_id}) и CHECK статусов
-- hold | captured | canceled | refunded — двойное списание
-- исключено на уровне схемы.

-- История платежей юзера (GET /api/v1/payments, MVP #7): свежие сверху.
CREATE INDEX IF NOT EXISTS idx_payments_user_created
    ON payments (user_id, created_at DESC);

-- Платёж поездки: холд/capture/чек по rental_id (gRPC PaymentOrchestrator).
CREATE INDEX IF NOT EXISTS idx_payments_rental
    ON payments (rental_id);

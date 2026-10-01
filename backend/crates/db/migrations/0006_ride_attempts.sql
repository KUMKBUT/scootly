-- MVP #6 (компенсация unlock-fail, ADR-0006): попытка разблокировки.
-- Пишется ДО отправки unlock; исход — ack замка за 10 c:
--   pending → acked  (поездка едет)
--   pending → failed (холд снят, юзер уведомлён, самокат offline)
-- Повторная попытка юзера — новая строка (у каждой свой исход).

CREATE TABLE IF NOT EXISTS ride_attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    rental_id UUID NOT NULL REFERENCES rentals (id),
    scooter_id UUID NOT NULL REFERENCES scooters (id),
    status VARCHAR(16) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'acked', 'failed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at TIMESTAMPTZ
);

-- История попыток по поездке (диагностика «самокат не открылся»).
CREATE INDEX IF NOT EXISTS idx_ride_attempts_rental
    ON ride_attempts (rental_id, created_at DESC);

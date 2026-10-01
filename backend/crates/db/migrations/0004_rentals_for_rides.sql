-- MVP #4 (поездка): старт из брони или напрямую, финиш (лок + расчёт).
-- Схема rentals из 0001 дополняется под контракт openapi `Ride`
-- (status: active | finished | failed, reservation_id, координаты финиша).

ALTER TABLE rentals ADD COLUMN reservation_id UUID REFERENCES bookings (id);

ALTER TABLE rentals ADD COLUMN finished_lat DOUBLE PRECISION;
ALTER TABLE rentals ADD COLUMN finished_lon DOUBLE PRECISION;

-- openapi: unlock не подтверждён → ride.status = failed (ADR-0006, MVP #6).
ALTER TABLE rentals DROP CONSTRAINT rentals_status_check;
ALTER TABLE rentals ADD CONSTRAINT rentals_status_check
    CHECK (status IN ('active', 'finished', 'failed'));

-- Не больше одной активной поездки на юзера (409 ride_in_progress) —
-- вторая линия после UPDATE..WHERE..RETURNING, как у броней (миграция 0002).
CREATE UNIQUE INDEX IF NOT EXISTS uq_active_ride_per_user
    ON rentals (user_id) WHERE status = 'active';

-- История поездок: свежие сверху, курсорная пагинация по started_at.
CREATE INDEX IF NOT EXISTS idx_rentals_user_started
    ON rentals (user_id, started_at DESC);

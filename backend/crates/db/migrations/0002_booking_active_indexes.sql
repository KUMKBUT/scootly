-- MVP #3 (бронирование, docs/mvp.md §5.1): частичные UNIQUE вместо полного
-- UNIQUE(scooter_id, status). Полный уникал позволял лишь одну строку на пару
-- (самокат, статус) навсегда — вторая историческая бронь того же самоката
-- ломала constraint. Теперь правила:
--   * не больше одной активной брони на самокат (гонка: вторая линия после
--     UPDATE scooters ... WHERE status='available' RETURNING, ADR-0003/0015);
--   * не больше одной активной брони на юзера (§5.1 «Лимит»).

ALTER TABLE bookings DROP CONSTRAINT IF EXISTS uq_active_booking_per_scooter;

CREATE UNIQUE INDEX IF NOT EXISTS uq_active_booking_per_scooter
    ON bookings (scooter_id) WHERE status = 'active';

CREATE UNIQUE INDEX IF NOT EXISTS uq_active_booking_per_user
    ON bookings (user_id) WHERE status = 'active';

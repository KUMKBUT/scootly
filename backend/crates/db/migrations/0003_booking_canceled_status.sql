-- MVP #3 (бронирование): ручная отмена брони. OpenAPI (docs/api/openapi.yaml,
-- Reservation.status) допускает `canceled`, а CHECK из 0001 — нет. Отменённая
-- бронь отличается от истёкшей: событие booking.expired.v1 несёт reason
-- `canceled` против `ttl` (websocket.md §4), рестарт сервиса восстанавливает
-- состояние из PG как есть.

ALTER TABLE bookings DROP CONSTRAINT bookings_status_check;

ALTER TABLE bookings ADD CONSTRAINT bookings_status_check
    CHECK (status IN ('active', 'expired', 'converted', 'canceled'));

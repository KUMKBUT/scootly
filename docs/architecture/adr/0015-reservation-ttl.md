# 0015. Бронирование: гонки, TTL и статус `canceled`

- Status: accepted
- Date: 2026-09-30
- Deciders: scootly team

## Context

MVP #3 (docs/mvp.md §2, §5.1) — бесплатная бронь самоката на 10 минут с
TTL-автоснятием и ручной отменой. Ограничения:

- двойная бронь одного самоката невозможна даже при параллельных запросах;
- PostgreSQL — source of truth, Redis — только TTL-триггер (ADR-0003);
- `bookings.status` из схемы 0001 допускал `active | expired | converted`,
  а контракт OpenAPI (`Reservation.status`) обещает ещё и `canceled`;
- `UNIQUE (scooter_id, status)` из 0001 — полный уникал: вторая историческая
  бронь того же самоката нарушала бы constraint (пара `(scooter, expired)`
  уже занята).

## Decision

1. **Захват самоката — атомарный**, без SELECT-then-UPDATE:
   `UPDATE scooters SET status='booked' WHERE id=$1 AND status='available'
   RETURNING id`. Проигравший гонку получает `409 scooter_unavailable`.
2. **Вторая линия обороны — частичные UNIQUE** (миграция 0002):
   `uq_active_booking_per_scooter (scooter_id) WHERE status='active'` и
   `uq_active_booking_per_user (user_id) WHERE status='active'`. Нарушение
   мапится в `409 scooter_unavailable` / `409 reservation_active_exists`.
   Лимит «одна активная бронь на юзера» проверяется до захвата самоката —
   повторный POST на тот же самокат даёт `reservation_active_exists`, а не
   гонку двух UNIQUE-нарушений; параллельные брони одного юзера закрывает сам
   индекс.
3. **TTL**: при создании бронь + outbox (`booking.created.v1`,
   `scooter.status.v1`) пишутся в одной транзакции; Redis-триггер
   `SET booking:ttl:{id} PX 600000` — best-effort после коммита. Снятие —
   фоновый джоб сверки rental-service (по умолчанию раз в 10 c,
   `BOOKING_SWEEP_INTERVAL_SECS`): истёкшие `active` → `expired`, их самокаты
   (если всё ещё `booked`) → `available`, outbox (`booking.expired.v1` c
   `reason=ttl`) — в той же транзакции. Redis недоступен → бронь всё равно
   создаётся, TTL догоняет джоб (§5.1 «Redis недоступен»).
4. **Ручная отмена — статус `canceled`** (миграция 0003 расширяет CHECK), не
   `expired`: контракт OpenAPI различает их, событие `booking.expired.v1`
   несёт `reason=canceled` (websocket.md §4: `ttl | canceled | converted`).
   `DELETE` идемпотентен: повтор — `204`, чужая/несуществующая бронь — `404`.

## Consequences

- Плюсы: двойная бронь невозможна по построению (атомарный UPDATE + индексы);
  рестарт rental-service ничего не теряет — состояние только из PG; отмена и
  TTL не отличаются для клиента по REST-контракту.
- Минусы: снятие брони по TTL отстаёт от `expires_at` на интервал джоба
  (допустимо: самокат в UI помечен `booked`, а не `available`); проверка
  лимита юзера — SELECT внутри транзакции, но финальную гарантию даёт именно
  частичный UNIQUE, а не SELECT.

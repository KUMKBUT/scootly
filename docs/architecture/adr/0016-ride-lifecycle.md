# 0016. Жизненный цикл поездки: старт из брони/напрямую, тариф, идемпотентный финиш

- Status: accepted
- Date: 2026-10-01
- Deciders: scootly team

## Context

MVP #4 (docs/mvp.md §2): поездка — старт из брони или напрямую (unlock самоката),
тик стоимости в UI, финиш (лок + расчёт). Контракт — openapi `Ride`
(active | finished | failed), события — asyncapi (`rental.started`, `rental.finished`).
Оплата (YooKassa холд → capture) — отдельная задача MVP #5, компенсация unlock-fail —
MVP #6 (ADR-0006), реальный MQTT-замок — вне MVP (ADR-0002).

## Decision

1. **Старт без race** — тот же приём, что у броней (ADR-0003):
   `UPDATE scooters SET status='rented' WHERE id=$1 AND status IN (...) RETURNING id`,
   никаких SELECT-then-UPDATE. Из брони — сначала конвертация
   (`UPDATE bookings SET status='converted' ... RETURNING`), сбой ниже откатывает её.
2. **Не больше одной активной поездки на юзера** — частичный UNIQUE
   `uq_active_ride_per_user` (миграция 0004) → `409 ride_in_progress`; проверка в
   транзакции до захвата самоката — для понятного кода ошибки, UNIQUE закрывает гонку.
3. **Тариф per_minute** — фикс разблокировки + цена минуты, значения в env
   (`TARIFF_UNLOCK_KOPEKS` / `TARIFF_PER_MIN_KOPEKS`, копейки). Неполная минута
   считается целой, минимум — 1 минута. Расчёт — чистая функция в rental-service:
   её же зеркалит клиент (shared/utils/tariff) для тика между снапшотами
   GET /rides/{id}; сервер — истина.
4. **Финиш идемпотентен**: лок подтверждается до фиксации (нет ack → `502
   lock_ack_timeout`, поездка остаётся активной), затем
   `UPDATE rentals .. WHERE status='active' RETURNING` — повторный вызов возвращает
   то же состояние без событий; двойного capture не будет и на уровне платежа
   (`UNIQUE idempotency_key = ride:{rental_id}`, ADR-0014, MVP #5).
5. **Шлюз замков — port** `LockGateway` с эмуляцией (`EmulatedLocks`, тег
   `Emulated` в workspace.dsl): команда всегда подтверждается. MVP #6 добавит
   реализацию с таймаутом 10 c; контракты 502 (`unlock_timeout`/`lock_ack_timeout`)
   и статус `failed` уже соблюдены.
6. **События через outbox** (ADR-0008), атомарно с изменениями:
   `rental.started.v1`, `rental.finished.v1`, `scooter.status.v1`, а при старте из
   брони — `booking.expired.v1` (reason=converted).
7. Схема: миграция 0004 — `rentals.reservation_id`, `finished_lat/lon`,
   статус `failed`, индексы (см. docs/db/schema.sql).

## Consequences

- Плюсы: полный платящий цикл без гонок и дублей; тариф меняется без релиза
  (env); замок и оплата — заменяемые порты, MVP #5/#6 ложатся без переделок.
- Минусы: до MVP #5 в `hold_id` пишется детерминированный ключ `hold:{rental_id}`
  вместо yookassa_id; тик в UI считается на клиенте — при расхождении часов
  снапшот GET /rides/{id} поправит сумму.

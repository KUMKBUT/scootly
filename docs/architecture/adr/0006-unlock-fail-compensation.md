# 0006. Компенсация при неудачном unlock (RideAttempt → void холда)

- Status: accepted
- Date: 2026-09-29
- Deciders: scootly team

## Context

Холд берётся **до** отправки unlock (ADR-0003). Замок может не подтвердить команду:
MQTT QoS 1, устройство офлайн/разряжено. Без компенсации деньги пользователя
остаются замороженными, поддержка получает тикет «списали, самокат не открылся».

## Decision

1. Новая сущность **RideAttempt** (`status: pending → acked | failed`) в БД
   rental-service — пишется до отправки unlock. Пользователь может повторить
   попытку — это новая RideAttempt (у каждой свой исход).
2. Ожидание ack от замка — **10 секунд** (таймаут команды `scooter/{id}/cmd`).
   Нет ack → `status=failed`.
3. Компенсация: **void холда** через payment-service (идемпотентно,
   `hold:{rental_id}`), RideAttempt → `failed`.
4. Событие `unlock_failed` в `rental.events.v1` (через outbox) →
   notification-service: «Аренда не началась, холд снят».
5. Наблюдаемость: метрика `unlock_failed_total` + dynamic view **UnlockFailed**
   в `docs/architecture/platform/workspace.dsl`.

## Consequences

- Плюсы: пользователь не теряет деньги, отказ — штатный сценарий с метрикой,
  а не тикет поддержки.
- Минусы: start аренды всегда ≥ 10 сек в плохом случае; нужна статус-машина
  RideAttempt и идемпотентный void. Самокат без ack остаётся «под вопросом» —
  перевод в offline решается оператором/телеметрией, не в этом ADR.

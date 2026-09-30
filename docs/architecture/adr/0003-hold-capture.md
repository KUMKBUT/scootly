# 0003. Холд → capture для поминутного тарифа + сверка

- Status: accepted (amended by ADR-0014: idempotency key capture = ride_id, без attempt)
- Date: 2026-09-29
- Deciders: scootly team

## Context

Поминутный тариф нельзя оплатить заранее (сумма неизвестна), подписка оплачивается сразу.
Нужна гарантия «поездка оплачена» без двойных списаний при обрывах связи после unlock.

## Decision

1. Старт аренды: **холд** через YooKassa (Payment Service — единственная точка интеграции).
   Финиш: **capture** на рассчитанную сумму (`tariffEngine`).
2. Каждый capture — с **idempotency key** `(rental_id, attempt)`; в БД —
   `UNIQUE (rental_id, attempt)` в `payment.payments`.
3. **Reconciliation job** раз в 5 минут: сверяет `rentals.status=finished` без `captured`
   платежа, повторяет capture; неудачные — в **DLQ** (`payment.dlq`, ручной разбор).
4. Webhooks YooKassa могут приходить **не по порядку** — состояние платежа выводится
   по `status` + времени события, а не по порядку доставки.

## Consequences

- Плюсы: стандарт индустрии, нет потерь выручки при обрывах, нет двойных списаний.
- Минусы: холд блокирует лимит карты; нужен воркер сверки и мониторинг DLQ
  (алерт в Telegram при росте).

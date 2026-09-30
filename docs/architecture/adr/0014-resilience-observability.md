# 0014. Отказоустойчивость и наблюдаемость: capture-ключ, circuit breaker, graceful shutdown, бизнес-метрики

- Status: accepted
- Date: 2026-09-29
- Deciders: scootly team

## Context

Упущенные при проектировании базовые требования надёжности: ключ идемпотентности
capture, поведение при недоступности YooKassa, остановка подов в K8s, бизнес-метрики
для дашбордов. Также уточняет ADR-0003.

## Decision

1. **Idempotency key на capture = ride_id**: формат `ride:{rental_id}`
   (для подписочных платежей — `sub:{user_id}:{period}`). **Аменда ADR-0003**:
   составной ключ `(rental_id, attempt)` больше не используется, колонка
   `attempt` выпилена из `payments`; `UNIQUE(idempotency_key)` в БД гарантирует
   «один capture на аренду». Ключ hold — `hold:{rental_id}` (отдельный вызов
   YooKassa, отдельный ключ), в БД не хранится — выводится детерминированно.
2. **Circuit breaker на исходящие вызовы YooKassa** в payment-service:
   `tower::limit` (лимит одновременных запросов + таймаут) + `governor` (rate
   limit). При open-состоянии capture/hold не роняют запрос юзера 500-й, а
   уходят в retry-очередь reconciliation (ADR-0003, сверка раз в 5 мин).
3. **Graceful shutdown во всех сервисах**: `axum::serve().with_graceful_shutdown()`
   на SIGTERM; фоновые воркеры (outbox relay, reconciliation, telemetry) добивают
   текущий батч и закрывают соединения. K8s: `terminationGracePeriodSeconds` ≥ 30.
4. **Бизнес-метрики в Prometheus** (рядом с техническими):
   `rides_started_total`, `unlock_failed_total` (ADR-0006), `battery_low_total`;
   дашборды Grafana и алерты строятся от них в первую очередь.

## Consequences

- Плюсы: двойное списание невозможно по построению; деградация эквайринга не
  видна юзеру как 500; рестарты подов без обрыва полётёверов; прод-дашборды
  отвечают на бизнес-вопросы («сколько стартов», «сколько отказов замка»).
- Минусы: breaker-состояние — in-memory на под (не распределённый) — приемлемо;
  reconciliation-очередь растёт при долгом open — мониторится глубиной.

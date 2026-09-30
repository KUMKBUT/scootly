# 0005. Kafka: retention по топикам и durability

- Status: accepted (имена топиков версионируются с ADR-0008: `<name>.v1`, DLQ — ADR-0010)
- Date: 2026-09-29
- Deciders: scootly team

## Context

Топики имеют разную ценность: телеметрия — потоковая (6300 устройств × 15 сек),
платежи/тикеты — критичные. Нужны retention под нагрузку пика 25 RPS с запасом до 50.

## Decision

Retention (см. также `x-kafka-retention` в `docs/api/asyncapi.yaml`):

| Топик | Retention | Обоснование |
|---|---|---|
| `rental.events` | 7 дней | Аудит аренд, реплей для сверки |
| `scooter.telemetry` | 24 часа | Поток, долговременно не нужен |
| `support.tickets` | 30 дней | Ручной разбор, споры |
| `payment.events` | 30 дней | Финансовая отчётность |
| `notification.send` | 3 дня | Переотправка при сбоях Bot API |
| `operator.tasks`, `shift.events`, `booking.expired`, `scooter.status` | 7 дней | Операционный аудит |

`min.insync.replicas=1` — **только для dev** (KRaft-одиночка в compose).
В проде: репликация ≥3, `min.insync.replicas=2`, `acks=all` на продюсерах
платежей и аренд.

## Consequences

- Плюсы: диск не растёт бесконечно, критичные события переживают рестарт консьюмеров.
- Минусы: телеметрию старше суток надо искать в DWH (пока нет — бэклог).

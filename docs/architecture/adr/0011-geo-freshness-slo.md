# 0011. Свежесть GEO-кэша: SLO p95 < 500 мс + fallback на last_position

- Status: accepted
- Date: 2026-09-29
- Deciders: scootly team

## Context

Карта в miniapp показывает самокаты из Redis GEO. Позиции доходят цепочкой
MQTT → telemetry-service → Kafka → scooter-service → Redis. Лаг цепочки = юзер
приходит к «фантомному» самокату. Также не был определён fallback при пустом/
недоступном GEO-кэше.

## Decision

1. **telemetry-service пишет в Redis GEO сам**, сразу после дедупа (ADR-0009),
   пайплайном: `GEOADD` + `EXPIRE` (TTL 60 сек — сохраняется). Путь в обход Kafka
   убирает самый медленный участок.
2. **SLO: p95 < 500 мс** от приёма MQTT до записи в Redis GEO. Метрика
   `telemetry_geo_write_seconds` (histogram), алерт на пробой SLO.
3. **Fallback**: если GEO-кэш не отвечает или по самокату нет записи — geo-service
   запрашивает **last_position у scooter-service по gRPC** (владелец таблицы;
   правило database-per-service не нарушаем). Scooter-service продолжает
   персистить позиции из Kafka в PostgreSQL.

## Alternatives considered

- Fallback прямым чтением чужой таблицы из geo-service — нарушает правило
  «не ходить в БД другого сервиса» (AGENT.md).
- Писать в Redis из scooter-service (как было): лаг выше на всю Kafka-цепочку.

## Consequences

- Плюсы: карта свежая (SLO измеряется, а не «как получится»), деградация
  предсказуема: nearby работает даже при пустом кэше.
- Минусы: sync-зависимость geo → scooter только в деградации (не на горячем пути);
  last_position из Postgres может отставать до секунды lag консюмера — приемлемо.

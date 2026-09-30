# 0001. Use Rust + Axum for backend

- Status: accepted
- Date: 2026-09-29
- Deciders: scootly team

## Context

Нужен backend для ~10 микросервисов аренды самокатов: высокая нагрузка на гео-запросы,
интеграции с Kafka/Redis/Postgres, маленькие Docker-образы, строгая типизация.

Рассматривались: Rust + Axum, Go + Gin, Node + NestJS.

## Decision

Используем **Rust + Axum** для всех микросервисов, общий код — в `backend/crates/*`
(`common`, `db`, `kafka`, `redis-client`, `proto`).

## Consequences

- Плюсы: производительность, безопасность памяти, один язык для всего backend, `sqlx` + `tokio` покрывают все нужды.
- Минусы: выше порог входа, дольше разработка MVP, нужен строгий `clippy`/`rustfmt` контроль.
- Миграции — только через `sqlx::migrate!` в `crates/db/migrations`.

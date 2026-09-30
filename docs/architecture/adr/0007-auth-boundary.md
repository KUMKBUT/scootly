# 0007. Граница авторизации: JWT в каждом сервисе, nginx — только TLS + rate limit

- Status: accepted
- Date: 2026-09-29
- Deciders: scootly team

## Context

NGINX Ingress маршрутизирует `/auth`, `/rentals`, `/scooters` и т.д. Возник вопрос:
валидировать ли JWT централизованно на ingress или в каждом сервисе. Также не была
определена точка проверки API-ключей ботов.

## Decision

1. **JWT** (из Telegram init-data, ADR — auth-service) валидируется **в каждом
   сервисе** через общий middleware `crates/common::auth` (проверка подписи и exp
   локально, без сетевого хопа; blacklist refresh-токенов — в Redis, как сейчас).
2. **NGINX** — только TLS-терминация, rate limiting и маршрутизация. Авторизацию
   не делает (никакой логики на ingress).
3. **API-ключи ботов**: выдача, ротация и отзыв — в auth-service; проверка — тот же
   middleware `common::auth` по **кэшу в Redis с TTL 5 мин** (ключ → разрешённые
   права). Промах кэша → gRPC-запрос в auth-service.

## Alternatives considered

- Проверка на ingress (ngx_http_auth_jwt): один пункт проверки, но вторая
  конфиг-система с собственной логикой и нет доступа к blacklist/правам из Redis.
- Sidecar/auth-proxy: лишний сетевой хоп на каждый запрос при 10 сервисах.

## Consequences

- Плюсы: единая логика в Rust-коде (типы, тесты, clippy), zero-latency проверки,
  ingress остаётся тупым и заменяемым.
- Минусы: middleware нельзя забыть (ловится интеграционным тестом на 401 в
  `tests/integration/`); кэш ключей ботов живёт до 5 мин после отзыва.

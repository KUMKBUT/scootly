*Scootly — pet project, day 2*

Карта живёт: самокаты рядом \+ live\-статусы по WebSocket\.

*Готово 2/8*
✅ 1\. Авторизация: Telegram `init-data` → JWT access \+ refresh, локальная проверка подписи \(ADR\-0007/0013\)
✅ 2\. Карта: `GET /scooters/nearby` из Redis GEO \(GEOSEARCH \+ HGETALL пайплайном\), fallback gRPC `last_position` по ADR\-0011, `ws-gateway`: `subscribe.scooters` \+ Redis pub/sub fan\-out, seeder — 12 самокатов Алматы\. 45 тестов, clippy и fmt — зелёные\.

*Осталось*
3\. Бронирование \(next\): 10 мин TTL, гонка — `UPDATE..WHERE..RETURNING`
4\. Поездка: unlock/finish
5\. Оплата: YooKassa hold→capture
6\. Компенсация unlock\-fail
7\. История поездок и платежей
8\. Outbox → Kafka

Стек: Rust \+ Axum · tonic gRPC · Redis · Postgres

\#rust \#axum \#redis \#petproject

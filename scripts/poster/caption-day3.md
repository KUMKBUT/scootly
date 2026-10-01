*Scootly — pet project, day 3*

Бронь без гонок: `POST /reservations` → 201, TTL 10 минут, конфликт решает Postgres\.

*Готово 3/8*
✅ 1\. Авторизация: Telegram `init-data` → JWT access \+ refresh \(ADR\-0007/0013\)
✅ 2\. Карта: `GET /scooters/nearby` из Redis GEO, fallback gRPC `last_position`, `ws-gateway` fan\-out \(ADR\-0011\)
✅ 3\. Бронирование: гонка — `UPDATE\.\.WHERE\.\.RETURNING` \+ частичные UNIQUE \(миграции 0002/0003\); TTL 10 мин — Redis\-триггер \+ джоб сверки из PG; лимит — одна активная бронь на юзера; outbox `booking\.created/expired\.v1` \(ADR\-0015\)\. Live\-контракт §5\.1 на Postgres — 8/8, всего 66 тестов, clippy и fmt — зелёные\.

*Осталось*
4\. Поездка: unlock/finish \(next\)
5\. Оплата: YooKassa hold→capture
6\. Компенсация unlock\-fail
7\. История поездок и платежей
8\. Outbox → Kafka

Стек: Rust \+ Axum · Postgres · Redis

\#rust \#axum \#postgres \#petproject

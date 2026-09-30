# Scootly — схема БД для MVP (этап 2)

Проект схемы первой версии: 5 таблиц `users`, `scooters`, `rides`, `reservations`, `payments`
(+ техническая `outbox`). PostgreSQL — **единственный source of truth** (в т.ч. для брони),
Redis — только TTL-триггер автоснятия и GEO-кэш (ADR-0003).

> Соответствие эталонной схеме: `rides` = `rentals`, `reservations` = `bookings` в
> [schema.sql](./schema.sql). При имплементе правим миграции в `backend/crates/db/migrations/`
> и синхронизируем `schema.sql`.

## 1. Правила проектирования

- Деньги — **копейки, `INT`** (никаких float).
- PK — `UUID` (`gen_random_uuid()`), время — `TIMESTAMPTZ`.
- Статусы — `VARCHAR + CHECK` (без отдельных enum-таблиц в MVP).
- Бронь без race: только `UPDATE scooters SET status='booked' WHERE id=$1 AND status='available' RETURNING id`,
  никаких SELECT-then-UPDATE; вторая линия — partial UNIQUE-индексы (ADR-0003).
- Идемпотентность платежей: `hold:{ride_id}` / `ride:{ride_id}` → `UNIQUE idempotency_key` (ADR-0003, ADR-0014).
- Доменные события — через `outbox` в одной транзакции с бизнес-данными.
- Прямой SQL — только внутри `crates/db` (репозитории), чужие таблицы сервисы не читают.

## 2. ER-обзор

```text
users 1───* reservations *───1 scooters
  │1                           │1
  │                            │
  └────* rides *───────────────┘
           │1
           └───* payments
outbox (техническая, пишется в транзакции с rides/payments/scooters)
```

## 3. DDL

```sql
CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Пользователь. Аутентификация через Telegram (ADR-0013), JWT-сессии в Redis.
CREATE TABLE users (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    telegram_id  BIGINT UNIQUE NOT NULL,
    phone        VARCHAR(20),              -- опционален в MVP
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Самокат. Гео-поиск идёт через Redis GEO; в PG координаты — «последнее известное» состояние.
CREATE TABLE scooters (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code         VARCHAR(32) UNIQUE NOT NULL,   -- номер на деке, вводится юзером как fallback к QR
    lat          DOUBLE PRECISION NOT NULL,
    lon          DOUBLE PRECISION NOT NULL,
    status       VARCHAR(16) NOT NULL DEFAULT 'available'
                 CHECK (status IN ('available', 'booked', 'rented', 'offline')),
    battery_pct  INT NOT NULL DEFAULT 100 CHECK (battery_pct BETWEEN 0 AND 100),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_scooters_status ON scooters (status) WHERE status = 'available';

-- Бронь (10 мин, бесплатная). У юзера максимум одна активная бронь,
-- у самоката — максимум одна активная бронь (partial UNIQUE как вторая линия после UPDATE..WHERE).
CREATE TABLE reservations (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     UUID NOT NULL REFERENCES users(id),
    scooter_id  UUID NOT NULL REFERENCES scooters(id),
    status      VARCHAR(16) NOT NULL DEFAULT 'active'
                CHECK (status IN ('active', 'expired', 'converted', 'canceled')),
    expires_at  TIMESTAMPTZ NOT NULL,          -- created_at + 10 мин; TTL-триггер в Redis
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX uq_active_reservation_per_scooter ON reservations (scooter_id) WHERE status = 'active';
CREATE UNIQUE INDEX uq_active_reservation_per_user    ON reservations (user_id)    WHERE status = 'active';
CREATE INDEX idx_reservations_expires ON reservations (expires_at) WHERE status = 'active';

-- Поездка. Создаётся ПОСЛЕ успешного холда; unlock-fail → status='failed' + отмена холда (ADR-0006).
CREATE TABLE rides (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id         UUID NOT NULL REFERENCES users(id),
    scooter_id      UUID NOT NULL REFERENCES scooters(id),
    reservation_id  UUID REFERENCES reservations(id),   -- если стартовали из брони
    tariff          VARCHAR(16) NOT NULL DEFAULT 'per_minute'
                    CHECK (tariff IN ('per_minute')),   -- в MVP один тариф
    status          VARCHAR(16) NOT NULL DEFAULT 'active'
                    CHECK (status IN ('active', 'finished', 'failed')),
    hold_id         TEXT NOT NULL,                 -- id холда в YooKassa (hold:{ride_id})
    started_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at     TIMESTAMPTZ,
    total_min       INT,
    amount_kopeks   INT,
    CHECK ( (status = 'active'  AND finished_at IS NULL)
         OR (status IN ('finished', 'failed') AND finished_at IS NOT NULL) )
);
CREATE INDEX idx_rides_user ON rides (user_id, started_at DESC);
-- У юзера не больше одной активной поездки:
CREATE UNIQUE INDEX uq_active_ride_per_user ON rides (user_id) WHERE status = 'active';

-- Платёж YooKassa: холд на старте → capture на финише (ADR-0003, ADR-0014).
CREATE TABLE payments (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id         UUID NOT NULL REFERENCES users(id),
    ride_id         UUID REFERENCES rides(id),
    yookassa_id     TEXT UNIQUE NOT NULL,          -- id платежа в YooKassa
    amount_kopeks   INT NOT NULL CHECK (amount_kopeks >= 0),
    status          VARCHAR(16) NOT NULL DEFAULT 'hold'
                    CHECK (status IN ('hold', 'captured', 'canceled', 'refunded')),
    idempotency_key TEXT UNIQUE NOT NULL,          -- hold:{ride_id} / ride:{ride_id}
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_payments_user ON payments (user_id, created_at DESC);
CREATE INDEX idx_payments_ride ON payments (ride_id);

-- Outbox: события публикует в Kafka отдельный воркер (ADR-0008, ADR-0010).
CREATE TABLE outbox (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    topic        VARCHAR(128) NOT NULL,            -- rental.events.v1, payment.events.v1, ...
    payload      JSONB NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    published_at TIMESTAMPTZ
);
CREATE INDEX idx_outbox_unpublished ON outbox (created_at) WHERE published_at IS NULL;
```

## 4. Статусные автоматы

**scooters.status**

```text
available ──бронь──▶ booked ──старт──▶ rented ──финиш──▶ available
    ▲                  │ttl/отмена         │
    └──────────────────┘                   │unlock-fail (ADR-0006)
    └───────────── offline ◀───────────────┘  (и ручной вывод оператором — после MVP)
```

Переходы `available → booked → rented` — атомарные `UPDATE ... WHERE status=<ожидаемый> RETURNING id`.

**reservations.status**: `active → converted` (стартовала поездка) | `expired` (TTL) | `canceled` (юзер).

**rides.status**: `active → finished` (успех) | `active → failed` (unlock не подтверждён за 10 c;
холд отменяется, `amount_kopeks = 0`).

**payments.status**: `hold → captured` (capture, идемпотентно по `ride:{ride_id}`) |
`hold → canceled` (unlock-fail или юзер не стартовал из брони) | `captured → refunded` (после MVP, поддержка).

## 5. Ключевые сценарии против схемы

**Бронирование (без race):**

```sql
UPDATE scooters SET status = 'booked'
WHERE id = $1 AND status = 'available'
RETURNING id;                       -- 0 строк → 409 Conflict
INSERT INTO reservations (...);      -- expires_at = now() + interval '10 minutes'
INSERT INTO outbox (topic, payload) VALUES ('scooter.status-changed', ...);  -- та же транзакция
```

**Финиш поездки (идемпотентный capture):**

```sql
UPDATE rides SET status='finished', finished_at=now(), total_min=$2, amount_kopeks=$3
WHERE id=$1 AND status='active' RETURNING *;   -- 0 строк → повторный finish, отдаем текущее состояние
UPDATE payments SET status='captured'
WHERE ride_id=$1 AND idempotency_key = 'ride:' || $1::text
  AND status='hold';                           -- UNIQUE дублирует защиту
```

**Снятие брони по TTL:** Redis-ключ истёк → джоб/воркер делает
`UPDATE reservations SET status='expired' WHERE id=$1 AND status='active' RETURNING scooter_id`,
затем тот же атомарный `UPDATE scooters ... WHERE status='booked'`. PG — истина, Redis — триггер (ADR-0003).

## 6. Что сознательно НЕ в схеме MVP

Карты/платёжные методы юзера (токен YooKassa хранится на стороне YooKassa, MVP хранит только
`yookassa_id`), подписки, тикеты, операторские задачи, телеметрия, routed-зоны —
см. эталон [schema.sql](./schema.sql) и [mvp.md](../mvp.md) (out of scope).

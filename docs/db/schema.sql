-- Scootly: эталонная схема БД (синхронизируется с crates/db/migrations).
-- Outbox-паттерн: события публикуются в Kafka отдельным воркером.
--
-- Правила (см. ADR-0003):
--   * PostgreSQL — source of truth, в т.ч. для брони. Redis — только TTL-триггер
--     автоснятия + фоновый джоб сверки (см. bookingManager).
--   * Бронирование — БЕЗ SELECT-then-UPDATE: только
--       UPDATE scooters SET status='booked' WHERE id=$1 AND status='available' RETURNING id
--     + UNIQUE-ограничения ниже как вторая линия обороны.
--   * Capture платежа идемпотентен: idempotency_key = ride:{rental_id}
--     (ADR-0003 в ред. ADR-0014); hold — hold:{rental_id}, выводится детерминированно.
--   * Офлайн-операции оператора идемпотентны: UNIQUE(client_op_id) (ADR-0012).

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

CREATE TABLE IF NOT EXISTS users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    telegram_id BIGINT UNIQUE NOT NULL,
    phone VARCHAR(20),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS scooters (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code VARCHAR(32) UNIQUE NOT NULL,
    lat DOUBLE PRECISION NOT NULL,
    lon DOUBLE PRECISION NOT NULL,
    status VARCHAR(16) NOT NULL DEFAULT 'available'
        CHECK (status IN ('available', 'booked', 'rented', 'offline')),
    battery_pct INT NOT NULL DEFAULT 100,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS scooter_batteries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    serial VARCHAR(64) UNIQUE NOT NULL,
    charge_pct INT NOT NULL DEFAULT 100,
    status VARCHAR(16) NOT NULL DEFAULT 'in_stock'
        CHECK (status IN ('in_stock', 'installed', 'charging', 'retired'))
);

CREATE TABLE IF NOT EXISTS rentals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id),
    scooter_id UUID NOT NULL REFERENCES scooters(id),
    tariff VARCHAR(16) NOT NULL DEFAULT 'per_minute'
        CHECK (tariff IN ('per_minute', 'package')),
    status VARCHAR(16) NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'finished')),
    hold_id TEXT NOT NULL,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ,
    total_min INT,
    amount_kopeks INT
);

CREATE TABLE IF NOT EXISTS bookings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id),
    scooter_id UUID NOT NULL REFERENCES scooters(id),
    status VARCHAR(16) NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'expired', 'converted', 'canceled')),
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- Частичные UNIQUE (миграция 0002, MVP #3): не больше одной активной брони
-- на самокат (вторая линия после UPDATE..WHERE..RETURNING) и на юзера (§5.1 «Лимит»).
CREATE UNIQUE INDEX IF NOT EXISTS uq_active_booking_per_scooter
    ON bookings (scooter_id) WHERE status = 'active';
CREATE UNIQUE INDEX IF NOT EXISTS uq_active_booking_per_user
    ON bookings (user_id) WHERE status = 'active';

CREATE TABLE IF NOT EXISTS payments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id),
    rental_id UUID REFERENCES rentals(id),
    yookassa_id TEXT UNIQUE NOT NULL,
    amount_kopeks INT NOT NULL,
    status VARCHAR(16) NOT NULL DEFAULT 'hold'
        CHECK (status IN ('hold', 'captured', 'canceled', 'refunded')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Идемпотентность capture: ride:{rental_id} / sub:{user_id}:{period} (ADR-0014).
    -- Один capture на аренду гарантируется UNIQUE.
    idempotency_key TEXT UNIQUE NOT NULL
);

CREATE TABLE IF NOT EXISTS subscriptions (
    user_id UUID PRIMARY KEY REFERENCES users(id),
    status VARCHAR(16) NOT NULL DEFAULT 'trial'
        CHECK (status IN ('trial', 'active', 'canceled')),
    is_trial BOOLEAN NOT NULL DEFAULT TRUE,
    trial_ends_at TIMESTAMPTZ,
    current_period_end TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- Trial-abuse: один триал на telegram_id (через users).
CREATE UNIQUE INDEX IF NOT EXISTS uq_single_trial
    ON subscriptions (user_id) WHERE is_trial = TRUE;

CREATE TABLE IF NOT EXISTS support_tickets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    source VARCHAR(8) NOT NULL CHECK (source IN ('bot', 'user')),
    author_ref TEXT NOT NULL,
    scooter_id UUID REFERENCES scooters(id),
    text TEXT NOT NULL,
    status VARCHAR(16) NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'in_progress', 'resolved')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS operator_tasks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Идемпотентность офлайн-операций RN-приложения (ADR-0012).
    client_op_id UUID UNIQUE NOT NULL,
    scooter_id UUID NOT NULL REFERENCES scooters(id),
    type VARCHAR(16) NOT NULL
        CHECK (type IN ('battery_replace', 'relocate', 'inspect')),
    status VARCHAR(16) NOT NULL DEFAULT 'created'
        CHECK (status IN ('created', 'assigned', 'completed')),
    assignee_id UUID REFERENCES users(id),
    photo_url TEXT,
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS outbox (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    topic VARCHAR(128) NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    published_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_outbox_unpublished ON outbox (created_at) WHERE published_at IS NULL;

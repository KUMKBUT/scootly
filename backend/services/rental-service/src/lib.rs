//! rental-service: брони (MVP #3), поездки (MVP #4) и компенсация
//! unlock-fail (MVP #6). PostgreSQL — source of truth, Redis — только
//! TTL-триггер, снятие — фоновый джоб сверки (ADR-0003, ADR-0015).

pub mod dto;
pub mod handlers;
pub mod services;

use axum::extract::FromRef;
use axum::Router;
use common::auth::JwtState;
use redis_client::LazyConnection;
use services::locks::Locks;
use services::payments::Payments;
use services::tariff::Tariff;
use sqlx::PgPool;

/// TTL брони: 10 минут, бесплатно (docs/mvp.md §2 #3).
pub const RESERVATION_TTL_SECS: i64 = 10 * 60;

/// Интервал фонового джоба сверки по умолчанию (ADR-0003): истёкшие брони
/// снимаются из PG даже без Redis-триггера.
pub const SWEEP_INTERVAL_SECS: u64 = 10;

/// openapi listRides: default 20, максимум 100.
pub const MAX_HISTORY_LIMIT: i64 = 100;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub jwt: JwtState,
    pub redis: LazyConnection,
    /// Тариф per_minute: фикс разблокировки + цена минуты (env, копейки).
    pub tariff: Tariff,
    /// Шлюз замков: эмуляция до проводки MQTT (ADR-0002); ack-таймаут 10 c и
    /// компенсация unlock-fail — MVP #6 (ADR-0006).
    pub locks: Locks,
    /// Шлюз оплаты: холд на старте, capture на финише (MVP #5, ADR-0003/0014).
    pub payments: Payments,
}

impl FromRef<AppState> for JwtState {
    fn from_ref(state: &AppState) -> JwtState {
        state.jwt.clone()
    }
}

pub fn router(state: AppState) -> Router {
    handlers::router(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservation_ttl_is_10_minutes() {
        assert_eq!(RESERVATION_TTL_SECS, 600);
    }

    #[test]
    fn history_limit_caps_at_openapi_maximum() {
        assert_eq!(MAX_HISTORY_LIMIT, 100);
    }
}

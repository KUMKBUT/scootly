//! rental-service: бронирование (MVP #3). PostgreSQL — source of truth,
//! Redis — только TTL-триггер, снятие — фоновый джоб сверки (ADR-0003, ADR-0015).

pub mod dto;
pub mod handlers;
pub mod services;

use axum::extract::FromRef;
use axum::Router;
use common::auth::JwtState;
use redis_client::LazyConnection;
use sqlx::PgPool;

/// TTL брони: 10 минут, бесплатно (docs/mvp.md §2 #3).
pub const RESERVATION_TTL_SECS: i64 = 10 * 60;

/// Интервал фонового джоба сверки по умолчанию (ADR-0003): истёкшие брони
/// снимаются из PG даже без Redis-триггера.
pub const SWEEP_INTERVAL_SECS: u64 = 10;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub jwt: JwtState,
    pub redis: LazyConnection,
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
}

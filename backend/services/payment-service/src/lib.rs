//! payment-service (MVP #5, #11): YooKassa — холд на старте поездки, capture на
//! финише (ADR-0003, ADR-0014). Единственная точка интеграции с эквайрингом.
//!
//! Слои: handlers (HTTP) / grpc (PaymentOrchestrator) → services → crates/db.
//! PostgreSQL — source of truth для платежей; идемпотентность —
//! `UNIQUE idempotency_key` (`hold:` / `ride:{rental_id}`). Шлюз [`YooKassa`]:
//! `Http` — staging/боевые ключи из env (MVP #11), `Emulated` — локальный стенд.

pub mod dto;
pub mod grpc;
pub mod handlers;
pub mod services;

use axum::extract::FromRef;
use axum::Router;
use common::auth::JwtState;
use services::yookassa::YooKassa;
use sqlx::PgPool;

/// openapi listPayments: default 20, максимум 100.
pub const MAX_HISTORY_LIMIT: i64 = 100;

/// Интервал джоба сверки по умолчанию (ADR-0003): повторяет незавершённые capture.
pub const RECONCILE_INTERVAL_SECS: u64 = 5 * 60;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub jwt: JwtState,
    /// Шлюз YooKassa: `Http` при заданных ключах (MVP #11), иначе эмуляция.
    pub yookassa: YooKassa,
}

impl FromRef<AppState> for JwtState {
    fn from_ref(state: &AppState) -> JwtState {
        state.jwt.clone()
    }
}

pub fn router(state: AppState) -> Router {
    handlers::router(state)
}

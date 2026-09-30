//! scooter-service: владелец таблицы `scooters` + gRPC last_position (ADR-0011).

pub mod grpc;

use axum::routing::get;
use axum::Router;
use sqlx::PgPool;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
}

pub fn http_router() -> Router {
    Router::new().route("/health", get(health))
}

async fn health() -> &'static str {
    "ok"
}

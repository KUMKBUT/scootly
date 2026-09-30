//! geo-service: `GET /api/v1/scooters/nearby` из Redis GEO-кэша (ADR-0011),
//! fallback — gRPC last_position у scooter-service.

pub mod dto;
pub mod handlers;
pub mod services;

use axum::extract::FromRef;
use axum::Router;
use common::auth::JwtState;
use proto::scootly::scooter::v1::scooter_positions_client::ScooterPositionsClient;
use redis_client::LazyConnection;

/// TTL хэша метаданных в Redis (свежесть выдачи, ADR-0011).
pub const CACHE_TTL_SECS: u64 = 60;

#[derive(Clone)]
pub struct AppState {
    pub jwt: JwtState,
    pub redis: LazyConnection,
    pub scooters: ScooterPositionsClient<tonic::transport::Channel>,
}

impl FromRef<AppState> for JwtState {
    fn from_ref(state: &AppState) -> JwtState {
        state.jwt.clone()
    }
}

pub fn router(state: AppState) -> Router {
    handlers::router(state)
}

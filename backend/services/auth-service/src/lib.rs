//! auth-service: Telegram init-data -> JWT (ADR-0007, ADR-0013).

pub mod dto;
pub mod handlers;
pub mod services;

use axum::extract::FromRef;
use axum::Router;
use common::auth::JwtState;
use sqlx::PgPool;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub jwt: JwtState,
    pub bot_token: String,
}

impl FromRef<AppState> for JwtState {
    fn from_ref(state: &AppState) -> JwtState {
        state.jwt.clone()
    }
}

pub fn router(state: AppState) -> Router {
    handlers::router(state)
}

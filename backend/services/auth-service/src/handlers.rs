//! Axum-роуты auth-service (handlers -> services -> crates/db).

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use common::auth::AuthUser;
use common::AppResult;

use crate::dto::{AuthResponse, RefreshRequest, TelegramLoginRequest};
use crate::services;
use crate::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/v1/auth/telegram", post(telegram_login))
        .route("/api/v1/auth/refresh", post(refresh))
        .route("/api/v1/me", get(me))
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

#[tracing::instrument(skip_all)]
async fn telegram_login(
    State(state): State<AppState>,
    Json(req): Json<TelegramLoginRequest>,
) -> AppResult<Json<AuthResponse>> {
    Ok(Json(services::auth::login_telegram(&state, &req).await?))
}

#[tracing::instrument(skip_all)]
async fn refresh(
    State(state): State<AppState>,
    Json(req): Json<RefreshRequest>,
) -> AppResult<Json<AuthResponse>> {
    Ok(Json(services::auth::refresh(&state, &req).await?))
}

#[tracing::instrument(skip_all)]
async fn me(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
) -> AppResult<Json<db::users::User>> {
    Ok(Json(db::users::find_by_id(&state.pool, claims.sub).await?))
}

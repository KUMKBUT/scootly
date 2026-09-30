//! Axum-роуты rental-service (handlers -> services -> crates).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use common::auth::AuthUser;
use common::AppResult;
use uuid::Uuid;

use crate::dto::{CreateReservation, ReservationDto};
use crate::services;
use crate::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/v1/reservations", post(create_reservation))
        .route("/api/v1/reservations/:id", delete(cancel_reservation))
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

#[tracing::instrument(skip_all, fields(user_id = %claims.sub))]
async fn create_reservation(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Json(req): Json<CreateReservation>,
) -> AppResult<(StatusCode, Json<ReservationDto>)> {
    let reservation = services::reservations::create(&state, claims.sub, req.scooter_id).await?;
    Ok((StatusCode::CREATED, Json(reservation)))
}

#[tracing::instrument(skip_all, fields(user_id = %claims.sub, reservation_id = %id))]
async fn cancel_reservation(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    services::reservations::cancel(&state, claims.sub, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

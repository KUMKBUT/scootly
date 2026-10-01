//! Axum-роуты rental-service (handlers -> services -> crates).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use common::auth::AuthUser;
use common::AppResult;
use uuid::Uuid;

use crate::dto::{
    CreateReservation, FinishRide, HistoryQuery, ReservationDto, RideDto, RidesPageDto, StartRide,
};
use crate::services;
use crate::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/v1/reservations", post(create_reservation))
        .route("/api/v1/reservations/:id", delete(cancel_reservation))
        .route("/api/v1/rides", get(list_rides).post(start_ride))
        .route("/api/v1/rides/:id", get(get_ride))
        .route("/api/v1/rides/:id/finish", post(finish_ride))
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

#[tracing::instrument(skip_all, fields(user_id = %claims.sub, scooter_id = %req.scooter_id))]
async fn start_ride(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Json(req): Json<StartRide>,
) -> AppResult<(StatusCode, Json<RideDto>)> {
    let rental = services::rides::start(&state, claims.sub, req).await?;
    Ok((
        StatusCode::CREATED,
        Json(RideDto::from_rental(rental, state.tariff)),
    ))
}

#[tracing::instrument(skip_all, fields(user_id = %claims.sub))]
async fn list_rides(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Query(query): Query<HistoryQuery>,
) -> AppResult<Json<RidesPageDto>> {
    let (items, next_before) =
        services::rides::history(&state, claims.sub, query.before, query.limit.unwrap_or(20))
            .await?;
    let items = items
        .into_iter()
        .map(|rental| RideDto::from_rental(rental, state.tariff))
        .collect();
    Ok(Json(RidesPageDto { items, next_before }))
}

#[tracing::instrument(skip_all, fields(user_id = %claims.sub, ride_id = %id))]
async fn get_ride(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<RideDto>> {
    let rental = services::rides::snapshot(&state, claims.sub, id).await?;
    Ok(Json(RideDto::from_rental(rental, state.tariff)))
}

#[tracing::instrument(skip_all, fields(user_id = %claims.sub, ride_id = %id))]
async fn finish_ride(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Path(id): Path<Uuid>,
    body: Option<Json<FinishRide>>,
) -> AppResult<Json<RideDto>> {
    let (lat, lon) = match body {
        Some(Json(req)) => req.validated()?,
        None => (None, None),
    };
    let rental = services::rides::finish(&state, claims.sub, id, lat, lon).await?;
    Ok(Json(RideDto::from_rental(rental, state.tariff)))
}

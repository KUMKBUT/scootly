//! Axum-роуты geo-service (handlers -> services -> crates).

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use common::auth::AuthUser;
use common::AppResult;

use crate::dto::{NearbyQuery, ScooterDto};
use crate::services;
use crate::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/v1/scooters/nearby", get(nearby))
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

#[tracing::instrument(skip_all)]
async fn nearby(
    State(state): State<AppState>,
    AuthUser(_claims): AuthUser,
    Query(query): Query<NearbyQuery>,
) -> AppResult<Json<Vec<ScooterDto>>> {
    let params = query.validate()?;
    let scooters =
        services::nearby::nearby(&state, params.lat, params.lon, params.radius_m).await?;
    Ok(Json(scooters))
}

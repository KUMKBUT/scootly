//! HTTP-роуты payment-service (openapi): методы оплаты, история платежей,
//! вебхук YooKassa. Холд/capture — через gRPC (grpc.rs), наружу их нет.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use common::auth::AuthUser;
use common::AppResult;

use crate::dto::{HistoryQuery, PaymentDto, PaymentMethodDto, WebhookNotification};
use crate::{services, AppState};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route(
            "/api/v1/payments/methods",
            get(list_methods).post(create_method),
        )
        .route("/api/v1/payments", get(list_payments))
        .route("/api/v1/payments/webhook", post(webhook))
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

/// `GET /api/v1/payments/methods`: привязанные карты (MVP — максимум одна).
async fn list_methods(
    State(state): State<AppState>,
    AuthUser(_): AuthUser,
) -> AppResult<Json<Vec<PaymentMethodDto>>> {
    Ok(Json(services::payments::list_methods(&state.yookassa)))
}

/// `POST /api/v1/payments/methods`: 201 + confirmation_url для Mini App.
#[tracing::instrument(skip_all, fields(user_id = %claims.sub))]
async fn create_method(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
) -> AppResult<(StatusCode, Json<serde_json::Value>)> {
    let url = services::payments::bind_method(&state, claims.sub).await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "confirmation_url": url })),
    ))
}

/// `GET /api/v1/payments`: история платежей (openapi listPayments).
#[tracing::instrument(skip_all, fields(user_id = %claims.sub))]
async fn list_payments(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Query(query): Query<HistoryQuery>,
) -> AppResult<Json<Vec<PaymentDto>>> {
    let items =
        services::payments::history(&state, claims.sub, query.ride_id, query.limit.unwrap_or(20))
            .await?
            .into_iter()
            .map(PaymentDto::from)
            .collect();
    Ok(Json(items))
}

/// `POST /api/v1/payments/webhook`: без JWT (openapi security: []),
/// подлинность — повторным запросом платежа в YooKassa. Всегда 200.
async fn webhook(State(state): State<AppState>, body: axum::body::Bytes) -> StatusCode {
    match serde_json::from_slice::<WebhookNotification>(&body) {
        Ok(notification) => services::payments::apply_webhook(&state, &notification).await,
        Err(error) => {
            // Плохое тело не роняет вебхук: YooKassa ретраит, мы логируем.
            tracing::warn!(%error, "invalid webhook body");
        }
    }
    StatusCode::OK
}

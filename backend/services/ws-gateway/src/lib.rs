//! ws-gateway: WebSocket-шлюз карты (docs/api/websocket.md).
//! Fan-out: Redis pub/sub `ws:scooters` → сессии с подпиской `subscribe.scooters`.
//! Source of truth — PG; после реконнекта клиент делает REST-снапшот.
//! Kafka → Redis pub/sub мост приходит с outbox-воркером (MVP #8).

pub mod fanout;
pub mod handlers;
pub mod protocol;
pub mod registry;

use std::sync::Arc;

use axum::extract::FromRef;
use axum::routing::get;
use axum::{middleware, Router};
use common::auth::JwtState;
use registry::Registry;

#[derive(Clone)]
pub struct AppState {
    pub jwt: JwtState,
    pub sessions: Arc<Registry>,
}

impl FromRef<AppState> for JwtState {
    fn from_ref(state: &AppState) -> JwtState {
        state.jwt.clone()
    }
}

pub fn router(state: AppState) -> Router {
    let auth = middleware::from_fn_with_state(state.clone(), handlers::auth_middleware);
    Router::new()
        .route("/health", get(handlers::health))
        .route("/api/v1/ws", get(handlers::ws_handler).route_layer(auth))
        .with_state(state)
}

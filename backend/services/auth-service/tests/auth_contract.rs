//! Контракт auth-service (MVP #1): /api/v1/auth/telegram, /api/v1/auth/refresh, /api/v1/me.
//!
//! Прогон: `cargo test --workspace`. Тесты, требующие Postgres, помечены
//! `#[ignore]` — запускаются после `make up && make migrate`:
//! `cargo test -p auth-service -- --ignored`.

use auth_service::{router, AppState};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::auth::JwtState;
use db::create_pool_lazy;
use http_body_util::BodyExt;
use serde_json::Value;
use std::sync::Arc;
use tower::util::ServiceExt;

const BOT_TOKEN: &str = "123456:TEST-TOKEN";
const JWT_SECRET: &str = "test-secret";

fn test_state() -> AppState {
    // Пул ленивый: коннекта к БД не будет, пока хендлер не тронет репозиторий.
    AppState {
        pool: create_pool_lazy("postgres://invalid:invalid@127.0.0.1:1/none").expect("lazy pool"),
        jwt: JwtState(Arc::new(JWT_SECRET.to_owned())),
        bot_token: BOT_TOKEN.to_owned(),
    }
}

async fn post_json(state: AppState, uri: &str, body: String) -> (StatusCode, Value) {
    let response = router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    to_parts(response).await
}

async fn get_auth(uri: &str, bearer: Option<&str>) -> StatusCode {
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response = router(test_state())
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    response.status()
}

async fn to_parts(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

fn json_body(init_data: &str) -> String {
    serde_json::json!({ "init_data": init_data }).to_string()
}

#[tokio::test]
async fn telegram_login_with_bad_signature_is_401() {
    let raw = "user=%7B%22id%22%3A1%7D&auth_date=1700000000&hash=deadbeef";
    let (status, body) = post_json(test_state(), "/api/v1/auth/telegram", json_body(raw)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
}

#[tokio::test]
async fn telegram_login_without_hash_is_401() {
    let raw = "user=%7B%22id%22%3A1%7D&auth_date=1700000000";
    let (status, _) = post_json(test_state(), "/api/v1/auth/telegram", json_body(raw)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn telegram_login_with_malformed_body_is_client_error() {
    let (status, _) = post_json(test_state(), "/api/v1/auth/telegram", "not-json".to_owned()).await;
    assert!(status.is_client_error(), "got {status}");
}

#[tokio::test]
async fn me_without_token_is_401() {
    assert_eq!(get_auth("/api/v1/me", None).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn me_with_garbage_token_is_401() {
    assert_eq!(
        get_auth("/api/v1/me", Some("garbage.token.here")).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn me_with_refresh_token_as_access_is_401() {
    let pair = common::auth::issue_pair(JWT_SECRET, uuid::Uuid::new_v4(), 42).unwrap();
    assert_eq!(
        get_auth("/api/v1/me", Some(&pair.refresh_token)).await,
        StatusCode::UNAUTHORIZED
    );
}

/// Полный цикл (user story №1): login -> JWT-пара -> /me -> refresh.
#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn full_login_refresh_me_cycle() {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL is not set");
    let state = AppState {
        pool: create_pool_lazy(&database_url).expect("pool"),
        jwt: JwtState(Arc::new(JWT_SECRET.to_owned())),
        bot_token: BOT_TOKEN.to_owned(),
    };

    let init_data = auth_service::services::telegram::build_signed_init_data(
        BOT_TOKEN,
        &[
            ("auth_date", &chrono::Utc::now().timestamp().to_string()),
            ("user", r#"{"id":4242,"username":"integration"}"#),
        ],
    );

    // 1. Login по init-data.
    let (status, body) = post_json(
        state.clone(),
        "/api/v1/auth/telegram",
        json_body(&init_data),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login failed: {body}");
    let access = body["access_token"]
        .as_str()
        .expect("access_token")
        .to_owned();
    let refresh = body["refresh_token"]
        .as_str()
        .expect("refresh_token")
        .to_owned();
    assert_eq!(body["user"]["telegram_id"], 4242);

    // 2. /me по access-токену.
    let response = router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/me")
                .header("authorization", format!("Bearer {access}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = to_parts(response).await;
    assert_eq!(status, StatusCode::OK, "me failed: {body}");
    assert_eq!(body["telegram_id"], 4242);

    // 3. Ротация пары по refresh-токену.
    let (status, rotated) = post_json(
        state,
        "/api/v1/auth/refresh",
        serde_json::json!({ "refresh_token": refresh }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "refresh failed: {rotated}");
    assert_ne!(rotated["access_token"], access, "access token must rotate");
}

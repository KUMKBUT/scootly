//! Контракт geo-service (MVP #2): GET /api/v1/scooters/nearby.
//!
//! Прогон: `cargo test --workspace`. Тест с живыми Redis + scooter-service
//! помечен `#[ignore]` — запускается после `make up && make seed`:
//! `cargo test -p geo-service -- --ignored`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::auth::JwtState;
use geo_service::{router, AppState, CACHE_TTL_SECS};
use http_body_util::BodyExt;
use proto::scootly::scooter::v1::scooter_positions_client::ScooterPositionsClient;
use redis_client::LazyConnection;
use serde_json::Value;
use std::sync::Arc;
use tonic::transport::Channel;
use tower::util::ServiceExt;

const JWT_SECRET: &str = "test-secret";

fn test_state() -> AppState {
    // Redis недоступен (порт 1) и gRPC-канал «мёртвый»: тесты деградации.
    let redis = LazyConnection::new("redis://127.0.0.1:1/7").expect("lazy redis");
    let channel = Channel::from_static("http://127.0.0.1:1").connect_lazy();
    AppState {
        jwt: JwtState(Arc::new(JWT_SECRET.to_owned())),
        redis,
        scooters: ScooterPositionsClient::new(channel),
    }
}

fn valid_token() -> String {
    common::auth::issue_pair(JWT_SECRET, uuid::Uuid::new_v4(), 1)
        .unwrap()
        .access_token
}

async fn get_nearby(state: AppState, query: &str, bearer: Option<&str>) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/scooters/nearby?{query}"));
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response = router(state)
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn nearby_without_token_is_401() {
    let (status, _) = get_nearby(test_state(), "lat=43.238&lon=76.889", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn nearby_with_garbage_token_is_401() {
    let (status, _) = get_nearby(
        test_state(),
        "lat=43.238&lon=76.889",
        Some("garbage.token.here"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn nearby_out_of_range_lat_is_400() {
    let (status, body) =
        get_nearby(test_state(), "lat=90.5&lon=76.889", Some(&valid_token())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
}

#[tokio::test]
async fn nearby_out_of_range_lon_is_400() {
    let (status, _) = get_nearby(test_state(), "lat=43.2&lon=200", Some(&valid_token())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn nearby_radius_over_max_is_400() {
    let (status, _) = get_nearby(
        test_state(),
        "lat=43.2&lon=76.9&radius_m=3001",
        Some(&valid_token()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn nearby_radius_zero_is_400() {
    let (status, _) = get_nearby(
        test_state(),
        "lat=43.2&lon=76.9&radius_m=0",
        Some(&valid_token()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn nearby_missing_lat_is_client_error() {
    let (status, _) = get_nearby(test_state(), "lon=76.889", Some(&valid_token())).await;
    assert!(status.is_client_error(), "got {status}");
}

/// Деградация (ADR-0011): cache unavailable + fallback unavailable → 200 и пустой список.
#[tokio::test]
async fn nearby_when_cache_and_fallback_down_is_empty_200() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let (status, body) =
        get_nearby(test_state(), "lat=43.238&lon=76.889", Some(&valid_token())).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body, Value::Array(Vec::new()));
}

/// Полный цикл (user story №1, карта): seed → nearby из Redis → тот же ответ
/// при выключенном fallback'е.
#[tokio::test]
#[ignore = "requires live Redis (make up) and seeded cache (make seed)"]
async fn nearby_serves_cache_snapshot() {
    let redis_url = std::env::var("REDIS_URL").expect("REDIS_URL is not set");
    let redis = LazyConnection::new(&redis_url).expect("lazy redis");

    // Готовим самокат в кэше тем же путём, что и seeder (ADR-0011 write path).
    let scooter = redis_client::geo::GeoScooter {
        id: uuid::Uuid::new_v4(),
        code: format!("GEO-TEST-{}", uuid::Uuid::new_v4().simple()),
        lat: 43.2380,
        lon: 76.8890,
        status: "available".to_owned(),
        battery_pct: 91,
    };
    let mut conn = redis.get().await.expect("redis connection");
    redis_client::geo::upsert(&mut conn, &scooter)
        .await
        .expect("geo upsert");

    // Fallback заведомо недоступен — ответ обязан прийти из кэша.
    let state = AppState {
        jwt: JwtState(Arc::new(JWT_SECRET.to_owned())),
        redis,
        scooters: ScooterPositionsClient::new(
            Channel::from_static("http://127.0.0.1:1").connect_lazy(),
        ),
    };
    let (status, body) = get_nearby(
        state,
        "lat=43.238&lon=76.889&radius_m=300",
        Some(&valid_token()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let items = body.as_array().expect("array").clone();
    assert!(
        items.iter().any(|s| s["code"] == scooter.code),
        "seeded scooter must be visible: {body}"
    );
    let ours = items
        .into_iter()
        .find(|s| s["code"] == scooter.code)
        .unwrap();
    assert_eq!(ours["status"], "available");
    assert_eq!(ours["battery_pct"], 91);

    // Хэш метаданных живёт CACHE_TTL_SECS (ADR-0011) — убираем за собой сразу.
    redis_client::geo::remove(&mut conn, scooter.id)
        .await
        .expect("cleanup");
}

#[test]
fn cache_ttl_matches_adr() {
    assert_eq!(CACHE_TTL_SECS, 60);
}

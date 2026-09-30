//! Контракт ws-gateway (MVP #2): auth по ?token=, роутинг подписок.
//!
//! Прогон: `cargo test --workspace`. Живой прогон с Redis pub/sub — после
//! `make up`: `cargo test -p ws-gateway -- --ignored`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::auth::JwtState;
use std::sync::Arc;
use tower::util::ServiceExt;
use ws_gateway::{router, AppState};

const JWT_SECRET: &str = "test-secret";

fn test_state() -> AppState {
    AppState {
        jwt: JwtState(Arc::new(JWT_SECRET.to_owned())),
        sessions: Arc::new(ws_gateway::registry::Registry::new()),
    }
}

fn valid_token() -> String {
    common::auth::issue_pair(JWT_SECRET, uuid::Uuid::new_v4(), 1)
        .unwrap()
        .access_token
}

async fn get_ws(token: Option<&str>) -> StatusCode {
    let mut uri = "http://test/api/v1/ws".to_owned();
    if let Some(token) = token {
        uri.push_str(&format!("?token={token}"));
    }
    router(test_state())
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("connection", "Upgrade")
                .header("upgrade", "websocket")
                .header("sec-websocket-version", "13")
                .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn ws_without_token_is_401() {
    assert_eq!(get_ws(None).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn ws_with_garbage_token_is_401() {
    assert_eq!(
        get_ws(Some("garbage.token.here")).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn ws_with_refresh_token_is_401() {
    let refresh = common::auth::issue_pair(JWT_SECRET, uuid::Uuid::new_v4(), 1)
        .unwrap()
        .refresh_token;
    assert_eq!(get_ws(Some(&refresh)).await, StatusCode::UNAUTHORIZED);
}

/// Валидный токен проходит auth-слой; дальше без полного handshake будет
/// клиентская ошибка апгрейда, но точно не 401.
#[tokio::test]
async fn ws_with_valid_token_passes_auth_layer() {
    let status = get_ws(Some(&valid_token())).await;
    assert!(
        status.is_client_error() && status != StatusCode::UNAUTHORIZED,
        "got {status}"
    );
}

/// Auth-токен чужим секретом не проходит.
#[tokio::test]
async fn ws_with_wrong_secret_token_is_401() {
    let foreign = common::auth::issue_pair("other-secret", uuid::Uuid::new_v4(), 1)
        .unwrap()
        .access_token;
    assert_eq!(get_ws(Some(&foreign)).await, StatusCode::UNAUTHORIZED);
}

/// Живой цикл fan-out (websocket.md §4): pub/sub сообщение → сессии по подписке.
/// Требует Redis: `make up`.
#[tokio::test]
#[ignore = "requires live Redis (make up)"]
async fn fanout_delivers_update_to_subscriber_via_pubsub() {
    let redis_url = std::env::var("REDIS_URL").expect("REDIS_URL is not set");

    let sessions = Arc::new(ws_gateway::registry::Registry::new());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let user = uuid::Uuid::new_v4();
    sessions.register(user, tx);
    sessions.set_subscription(
        user,
        Some(ws_gateway::protocol::Subscription {
            lat: 43.238,
            lon: 76.889,
            radius_m: 500.0,
        }),
    );

    let registry = sessions.clone();
    let worker = tokio::spawn(async move { ws_gateway::fanout::run(redis_url, registry).await });

    // Даём воркеру подписаться.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let event = serde_json::json!({
        "type": "scooter.updated",
        "id": uuid::Uuid::new_v4(),
        "ts": "2026-09-30T12:00:00Z",
        "payload": {
            "id": uuid::Uuid::new_v4(),
            "lat": 43.2382,
            "lon": 76.8892,
            "status": "available",
            "battery_pct": 77
        }
    });
    let client = redis_client::connect(&std::env::var("REDIS_URL").unwrap())
        .await
        .unwrap();
    let mut conn = client.get_multiplexed_async_connection().await.unwrap();
    redis::cmd("PUBLISH")
        .arg(redis_client::geo::WS_SCOOTERS_CHANNEL)
        .arg(event.to_string())
        .query_async::<_, i64>(&mut conn)
        .await
        .unwrap();

    let received = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("timeout waiting fan-out");
    match received {
        Some(axum::extract::ws::Message::Text(text)) => {
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(value["type"], "scooter.updated");
            assert_eq!(value["payload"]["battery_pct"], 77);
        }
        other => panic!("expected text message, got {other:?}"),
    }

    worker.abort();
}

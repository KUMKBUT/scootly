//! Контракт rental-service (MVP #3): POST/DELETE /api/v1/reservations + джоб сверки.
//!
//! Прогон: `cargo test --workspace`. Тесты, требующие Postgres, помечены
//! `#[ignore]` — запускаются после `make up && make migrate`:
//! `cargo test -p rental-service -- --ignored`.
//!
//! Кейсы docs/mvp.md §5.1: race, TTL, повторный запрос, Redis недоступен
//! (Redis в тестах всегда недоступен — бронь всё равно создаётся), rollback
//! транзакции, лимит активной брони на юзера. Рестарт сервиса покрыт тем,
//! что состояние читается только из PG.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::auth::JwtState;
use db::create_pool_lazy;
use http_body_util::BodyExt;
use rental_service::{router, AppState, RESERVATION_TTL_SECS};
use serde_json::Value;
use sqlx::PgPool;
use std::sync::Arc;
use tower::util::ServiceExt;
use uuid::Uuid;

const JWT_SECRET: &str = "test-secret";
const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

fn test_state() -> AppState {
    // Пул и Redis ленивые и заведомо недоступные: контракт должен держаться
    // без БД (auth) и без Redis (бронь живёт в PG, ADR-0003).
    AppState {
        pool: create_pool_lazy("postgres://invalid:invalid@127.0.0.1:1/none").expect("lazy pool"),
        jwt: JwtState(Arc::new(JWT_SECRET.to_owned())),
        redis: redis_client::LazyConnection::new("redis://127.0.0.1:1/7").expect("lazy redis"),
    }
}

fn bearer(user_id: Uuid) -> String {
    common::auth::issue_pair(JWT_SECRET, user_id, 1)
        .unwrap()
        .access_token
}

async fn post_reservation(state: &AppState, token: &str, scooter_id: Uuid) -> (StatusCode, Value) {
    let body = serde_json::json!({ "scooter_id": scooter_id }).to_string();
    let response = router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/reservations")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    to_parts(response).await
}

async fn delete_reservation(state: &AppState, token: &str, id: Uuid) -> (StatusCode, Value) {
    let response = router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/reservations/{id}"))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    to_parts(response).await
}

async fn to_parts(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn create_without_token_is_401() {
    let response = router(test_state())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/reservations")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"scooter_id":"00000000-0000-0000-0000-000000000000"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn create_with_garbage_token_is_401() {
    let (status, _) = post_reservation(&test_state(), "garbage.token.here", Uuid::new_v4()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn cancel_without_token_is_401() {
    let response = router(test_state())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/reservations/{}", Uuid::new_v4()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn create_with_malformed_body_is_client_error() {
    let token = bearer(Uuid::new_v4());
    let response = router(test_state())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/reservations")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from("not-json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        response.status().is_client_error(),
        "got {}",
        response.status()
    );
}

#[test]
fn reservation_ttl_matches_mvp() {
    assert_eq!(RESERVATION_TTL_SECS, 10 * 60);
    assert_eq!(SWEEP_INTERVAL, std::time::Duration::from_secs(10));
}

// ── Живой Postgres: `cargo test -p rental-service -- --ignored` ────────────

struct Fixture {
    pool: PgPool,
    state: AppState,
    user_id: Uuid,
    token: String,
    scooter_codes: Vec<String>,
}

impl Fixture {
    async fn setup() -> Self {
        let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL is not set");
        let pool = create_pool_lazy(&database_url).expect("pool");
        let state = AppState {
            pool: pool.clone(),
            jwt: JwtState(Arc::new(JWT_SECRET.to_owned())),
            // Redis недоступен намеренно: бронь обязана работать без него (§5.1).
            redis: redis_client::LazyConnection::new("redis://127.0.0.1:1/7").expect("lazy redis"),
        };
        let user_id = setup_user(&pool).await;
        Self {
            pool,
            state,
            user_id,
            token: bearer(user_id),
            scooter_codes: Vec::new(),
        }
    }

    async fn scooter(&mut self) -> db::scooters::Scooter {
        let code = format!("RT-{}", &Uuid::new_v4().simple().to_string()[..12]);
        let scooter = db::scooters::upsert_by_code(
            &self.pool,
            &db::scooters::NewScooter {
                code: code.clone(),
                lat: 43.238,
                lon: 76.889,
                status: "available".to_owned(),
                battery_pct: 80,
            },
        )
        .await
        .expect("seed scooter");
        self.scooter_codes.push(code);
        scooter
    }
}

async fn teardown(fx: &Fixture) {
    let _ = db::bookings::delete_by_user_ids(&fx.pool, &[fx.user_id]).await;
    let _ = db::scooters::delete_by_codes(&fx.pool, &fx.scooter_codes).await;
}

async fn setup_user(pool: &PgPool) -> Uuid {
    let telegram_id = (Uuid::new_v4().as_u128() % (i64::MAX as u128)) as i64;
    db::users::upsert_by_telegram_id(pool, telegram_id, None)
        .await
        .expect("seed user")
        .id
}

/// §5.1 «Rollback транзакции» + полный цикл: 201 → booked → 204 → available.
#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn full_cycle_create_and_cancel() {
    let mut fx = Fixture::setup().await;
    let scooter = fx.scooter().await;

    let (status, body) = post_reservation(&fx.state, &fx.token, scooter.id).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["scooter_id"], scooter.id.to_string());
    assert_eq!(body["status"], "active");
    let reservation_id: Uuid = body["id"].as_str().expect("id").parse().unwrap();
    let booking = db::bookings::find_by_id(&fx.pool, reservation_id)
        .await
        .expect("booking");
    // created_at ставит БД, expires_at — приложение: допускаем рассинхрон часов.
    let ttl = (booking.expires_at - booking.created_at).num_seconds();
    assert!(
        (ttl - RESERVATION_TTL_SECS).abs() <= 1,
        "ttl must be ~{RESERVATION_TTL_SECS}s, got {ttl}s"
    );
    assert_eq!(
        db::scooters::find_by_id(&fx.pool, scooter.id)
            .await
            .unwrap()
            .status,
        "booked"
    );

    // Отмена: 204, самокат снова available, повтор — идемпотентные 204.
    let (status, body) = delete_reservation(&fx.state, &fx.token, reservation_id).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");
    let (status, _) = delete_reservation(&fx.state, &fx.token, reservation_id).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        db::scooters::find_by_id(&fx.pool, scooter.id)
            .await
            .unwrap()
            .status,
        "available"
    );

    teardown(&fx).await;
}

/// §5.1 «Race»: 2 параллельных POST на один самокат → 1×201, 1×409.
#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn race_two_creates_one_wins() {
    let mut fx = Fixture::setup().await;
    let scooter = fx.scooter().await;
    let rival = setup_user(&fx.pool).await;
    let rival_token = bearer(rival);

    let (first, second) = tokio::join!(
        post_reservation(&fx.state, &fx.token, scooter.id),
        post_reservation(&fx.state, &rival_token, scooter.id),
    );
    // §5.1: 1×201, 1×409 — кто именно выиграет, неважно.
    let one_won = (first.0 == StatusCode::CREATED && second.0 == StatusCode::CONFLICT)
        || (first.0 == StatusCode::CONFLICT && second.0 == StatusCode::CREATED);
    assert!(
        one_won,
        "first: {} {}, second: {} {}",
        first.0, first.1, second.0, second.1
    );
    let loser = if first.0 == StatusCode::CONFLICT {
        &first.1
    } else {
        &second.1
    };
    assert_eq!(loser["code"], "scooter_unavailable");

    // Проигравший не оставил следов: самокат booked, у rival активной брони нет.
    assert_eq!(
        db::scooters::find_by_id(&fx.pool, scooter.id)
            .await
            .unwrap()
            .status,
        "booked"
    );
    let _ = db::bookings::delete_by_user_ids(&fx.pool, &[rival]).await;
    teardown(&fx).await;
}

/// §5.1 «Лимит» + «Rollback транзакции»: вторая бронь юзера → 409,
/// второй самокат остаётся available, ни брони, ни outbox-записи.
#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn second_booking_of_user_is_409_and_rolls_back() {
    let mut fx = Fixture::setup().await;
    let first = fx.scooter().await;
    let second_scooter = fx.scooter().await;

    let (status, body) = post_reservation(&fx.state, &fx.token, first.id).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let outbox_before = db::outbox::recent(&fx.pool, 100).await.unwrap().len();
    let (status, body) = post_reservation(&fx.state, &fx.token, second_scooter.id).await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(body["code"], "reservation_active_exists");

    // Rollback: самокат не захвачен, новые записи outbox не появились.
    assert_eq!(
        db::scooters::find_by_id(&fx.pool, second_scooter.id)
            .await
            .unwrap()
            .status,
        "available"
    );
    let outbox_after = db::outbox::recent(&fx.pool, 100).await.unwrap().len();
    assert_eq!(outbox_before, outbox_after, "failed create must not outbox");

    teardown(&fx).await;
}

/// §5.1 «Повторный запрос»: повторный POST на тот же самокат не плодит дублей.
#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn repeat_create_is_409_without_duplicates() {
    let mut fx = Fixture::setup().await;
    let scooter = fx.scooter().await;

    let (status, _) = post_reservation(&fx.state, &fx.token, scooter.id).await;
    assert_eq!(status, StatusCode::CREATED);

    // Тот же юзер — 409 reservation_active_exists (одна активная бронь на юзера)...
    let (status, body) = post_reservation(&fx.state, &fx.token, scooter.id).await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(body["code"], "reservation_active_exists");

    // ...и другой юзер получает scooter_unavailable на тот же самокат.
    let rival = setup_user(&fx.pool).await;
    let (status, body) = post_reservation(&fx.state, &bearer(rival), scooter.id).await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(body["code"], "scooter_unavailable");

    teardown(&fx).await;
}

#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn create_unknown_scooter_is_404() {
    let fx = Fixture::setup().await;
    let (status, body) = post_reservation(&fx.state, &fx.token, Uuid::new_v4()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(body["code"], "not_found");
}

/// §5.1 «TTL»: истёкшая бронь → expired, самокат снова available,
/// после сверки юзер может бронировать снова.
#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn ttl_sweep_expires_booking_and_frees_scooter() {
    let mut fx = Fixture::setup().await;
    let scooter = fx.scooter().await;

    // Бронь, уже истёкшая на момент создания (TTL-триггер «проспал» бы её).
    let booking = db::bookings::create(
        &fx.pool,
        fx.user_id,
        scooter.id,
        chrono::Duration::seconds(-1),
    )
    .await
    .expect("seed expired booking");
    assert_eq!(
        db::scooters::find_by_id(&fx.pool, scooter.id)
            .await
            .unwrap()
            .status,
        "booked"
    );

    let swept = rental_service::services::reservations::sweep_expired(&fx.state)
        .await
        .expect("sweep");
    assert!(swept >= 1);

    assert_eq!(
        db::bookings::find_by_id(&fx.pool, booking.id)
            .await
            .unwrap()
            .status,
        "expired"
    );
    assert_eq!(
        db::scooters::find_by_id(&fx.pool, scooter.id)
            .await
            .unwrap()
            .status,
        "available"
    );

    // После сверки бронирование работает снова.
    let (status, body) = post_reservation(&fx.state, &fx.token, scooter.id).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    teardown(&fx).await;
}

#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn cancel_foreign_reservation_is_404() {
    let mut fx = Fixture::setup().await;
    let scooter = fx.scooter().await;
    let (status, body) = post_reservation(&fx.state, &fx.token, scooter.id).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let reservation_id: Uuid = body["id"].as_str().expect("id").parse().unwrap();

    let outsider = bearer(Uuid::new_v4());
    let (status, _) = delete_reservation(&fx.state, &outsider, reservation_id).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Бронь не пострадала.
    assert_eq!(
        db::bookings::find_by_id(&fx.pool, reservation_id)
            .await
            .unwrap()
            .status,
        "active"
    );

    teardown(&fx).await;
}

/// Outbox атомарен с бронёй: created + status booked; отмена — expired(reason=canceled).
#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn outbox_records_booking_lifecycle() {
    let mut fx = Fixture::setup().await;
    let scooter = fx.scooter().await;

    let (status, body) = post_reservation(&fx.state, &fx.token, scooter.id).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let reservation_id: Uuid = body["id"].as_str().expect("id").parse().unwrap();

    let events = db::outbox::recent(&fx.pool, 20).await.unwrap();
    let created = events
        .iter()
        .find(|e| {
            e.topic == "booking.created.v1" && e.payload["booking_id"] == reservation_id.to_string()
        })
        .expect("booking.created.v1 in outbox");
    assert_eq!(created.payload["scooter_id"], scooter.id.to_string());
    assert!(events.iter().any(|e| {
        e.topic == "scooter.status.v1"
            && e.payload["scooter_id"] == scooter.id.to_string()
            && e.payload["status"] == "booked"
    }));

    let (status, delete_body) = delete_reservation(&fx.state, &fx.token, reservation_id).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {delete_body}");

    let events = db::outbox::recent(&fx.pool, 20).await.unwrap();
    let canceled = events
        .iter()
        .find(|e| {
            e.topic == "booking.expired.v1" && e.payload["booking_id"] == reservation_id.to_string()
        })
        .expect("booking.expired.v1 in outbox");
    assert_eq!(canceled.payload["reason"], "canceled");
    assert!(events.iter().any(|e| {
        e.topic == "scooter.status.v1"
            && e.payload["scooter_id"] == scooter.id.to_string()
            && e.payload["status"] == "available"
    }));

    teardown(&fx).await;
}

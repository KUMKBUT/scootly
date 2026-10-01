//! Контракт rental-service (MVP #4): POST/GET /api/v1/rides, POST /rides/{id}/finish.
//!
//! Прогон: `cargo test --workspace`. Тесты, требующие Postgres, помечены
//! `#[ignore]` — запускаются после `make up && make migrate`:
//! `cargo test -p rental-service -- --ignored`.
//!
//! Кейсы docs/mvp.md §2 #4: старт напрямую и из брони (converted), тик
//! стоимости в снапшоте, финиш (лок + расчёт) идемпотентен, без SELECT-then-UPDATE.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::auth::JwtState;
use db::create_pool_lazy;
use http_body_util::BodyExt;
use rental_service::services::tariff::Tariff;
use rental_service::{router, AppState};
use serde_json::Value;
use sqlx::PgPool;
use std::sync::Arc;
use tower::util::ServiceExt;
use uuid::Uuid;

const JWT_SECRET: &str = "test-secret";
const UNLOCK_KOPEKS: i32 = 2900;
const PER_MIN_KOPEKS: i32 = 800;

fn test_state() -> AppState {
    AppState {
        pool: create_pool_lazy("postgres://invalid:invalid@127.0.0.1:1/none").expect("lazy pool"),
        jwt: JwtState(Arc::new(JWT_SECRET.to_owned())),
        redis: redis_client::LazyConnection::new("redis://127.0.0.1:1/7").expect("lazy redis"),
        tariff: Tariff {
            unlock_kopeks: UNLOCK_KOPEKS,
            per_min_kopeks: PER_MIN_KOPEKS,
        },
        locks: Default::default(),
    }
}

fn bearer(user_id: Uuid) -> String {
    common::auth::issue_pair(JWT_SECRET, user_id, 1)
        .unwrap()
        .access_token
}

async fn send(
    state: &AppState,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<String>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"));
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder.body(Body::from(body.unwrap_or_default())).unwrap();
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn post_json(state: &AppState, uri: &str, token: &str, body: String) -> (StatusCode, Value) {
    send(state, "POST", uri, token, Some(body)).await
}

#[tokio::test]
async fn start_without_token_is_401() {
    let response = router(test_state())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/rides")
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
async fn finish_and_get_without_token_are_401() {
    for (method, uri) in [
        ("GET", format!("/api/v1/rides/{}", Uuid::new_v4())),
        ("POST", format!("/api/v1/rides/{}/finish", Uuid::new_v4())),
    ] {
        let response = router(test_state())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(&uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
}

#[tokio::test]
async fn start_with_garbage_token_is_401() {
    let (status, _) = post_json(
        &test_state(),
        "/api/v1/rides",
        "garbage.token.here",
        serde_json::json!({ "scooter_id": Uuid::new_v4() }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn start_with_malformed_body_is_client_error() {
    let token = bearer(Uuid::new_v4());
    let response = router(test_state())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/rides")
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
fn finish_coordinates_must_be_pair_in_range() {
    use rental_service::dto::FinishRide;

    let ok: (Option<f64>, Option<f64>) = FinishRide {
        lat: Some(43.2),
        lon: Some(76.9),
    }
    .validated()
    .unwrap();
    assert_eq!(ok, (Some(43.2), Some(76.9)));

    let none: (Option<f64>, Option<f64>) = FinishRide {
        lat: None,
        lon: None,
    }
    .validated()
    .unwrap();
    assert_eq!(none, (None, None));

    assert!(FinishRide {
        lat: Some(43.2),
        lon: None,
    }
    .validated()
    .is_err());
    assert!(FinishRide {
        lat: Some(200.0),
        lon: Some(0.0),
    }
    .validated()
    .is_err());
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
            redis: redis_client::LazyConnection::new("redis://127.0.0.1:1/7").expect("lazy redis"),
            tariff: Tariff {
                unlock_kopeks: UNLOCK_KOPEKS,
                per_min_kopeks: PER_MIN_KOPEKS,
            },
            locks: Default::default(),
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
        let code = format!("RD-{}", &Uuid::new_v4().simple().to_string()[..12]);
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

async fn setup_user(pool: &PgPool) -> Uuid {
    let telegram_id = (Uuid::new_v4().as_u128() % (i64::MAX as u128)) as i64;
    db::users::upsert_by_telegram_id(pool, telegram_id, None)
        .await
        .expect("seed user")
        .id
}

async fn teardown(fx: &Fixture) {
    let _ = db::rentals::delete_by_user_ids(&fx.pool, &[fx.user_id]).await;
    let _ = db::bookings::delete_by_user_ids(&fx.pool, &[fx.user_id]).await;
    let _ = db::scooters::delete_by_codes(&fx.pool, &fx.scooter_codes).await;
}

/// Полный цикл прямого старта: 201 → rented → тик растёт → финиш 200
/// (лок + расчёт) → available → повторный финиш идемпотентен.
#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn direct_ride_lifecycle_tick_and_idempotent_finish() {
    let mut fx = Fixture::setup().await;
    let scooter = fx.scooter().await;

    let (status, body) = post_json(
        &fx.state,
        "/api/v1/rides",
        &fx.token,
        serde_json::json!({ "scooter_id": scooter.id }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["status"], "active");
    assert_eq!(body["scooter_id"], scooter.id.to_string());
    assert!(body["reservation_id"].is_null());
    let ride_id: Uuid = body["id"].as_str().expect("id").parse().unwrap();
    assert_eq!(
        db::scooters::find_by_id(&fx.pool, scooter.id)
            .await
            .unwrap()
            .status,
        "rented"
    );

    // Тик: снапшот активной поездки показывает минуты и стоимость,
    // ничего не фиксируя (finished_at остаётся пустым).
    let (status, tick) = send(
        &fx.state,
        "GET",
        &format!("/api/v1/rides/{ride_id}"),
        &fx.token,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {tick}");
    assert_eq!(tick["status"], "active");
    let first_amount = tick["amount_kopeks"].as_i64().expect("amount");
    let first_min = tick["total_min"].as_i64().expect("total_min");
    assert_eq!(
        first_amount,
        i64::from(UNLOCK_KOPEKS) + i64::from(PER_MIN_KOPEKS) * first_min
    );
    assert!(tick["finished_at"].is_null());

    // Финиш: лок + расчёт, самокат вернулся в выдачу.
    let (status, finished) = post_json(
        &fx.state,
        &format!("/api/v1/rides/{ride_id}/finish"),
        &fx.token,
        serde_json::json!({ "lat": 43.238, "lon": 76.889 }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {finished}");
    assert_eq!(finished["status"], "finished");
    let total_min = finished["total_min"].as_i64().expect("total_min");
    assert_eq!(
        finished["amount_kopeks"],
        i64::from(UNLOCK_KOPEKS) + i64::from(PER_MIN_KOPEKS) * total_min
    );
    assert!(!finished["finished_at"].is_null());
    assert_eq!(
        db::scooters::find_by_id(&fx.pool, scooter.id)
            .await
            .unwrap()
            .status,
        "available"
    );
    let stored = db::rentals::find_owned(&fx.pool, fx.user_id, ride_id)
        .await
        .unwrap();
    assert_eq!(stored.finished_lat, Some(43.238));
    assert_eq!(stored.finished_lon, Some(76.889));

    // Повторный финиш: тот же результат, событий нет (идемпотентность).
    let (status, repeat) = post_json(
        &fx.state,
        &format!("/api/v1/rides/{ride_id}/finish"),
        &fx.token,
        serde_json::json!({}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {repeat}");
    assert_eq!(repeat["amount_kopeks"], finished["amount_kopeks"]);
    assert_eq!(repeat["finished_at"], finished["finished_at"]);
    let finish_events = db::outbox::recent(&fx.pool, 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| {
            e.topic == "rental.finished.v1" && e.payload["rental_id"] == ride_id.to_string()
        })
        .count();
    assert_eq!(finish_events, 1, "double finish must not double-publish");

    // История поездок: одна запись.
    let (status, page) = send(&fx.state, "GET", "/api/v1/rides", &fx.token, None).await;
    assert_eq!(status, StatusCode::OK, "body: {page}");
    assert_eq!(page["items"].as_array().expect("items").len(), 1);
    assert!(page["next_before"].is_null());

    teardown(&fx).await;
}

/// Старт из брони: бронь → converted, поездка с reservation_id; повторный
/// старт того же самоката другим юзером → 409 scooter_unavailable.
#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn start_from_reservation_converts_booking() {
    let mut fx = Fixture::setup().await;
    let scooter = fx.scooter().await;
    let booking = db::bookings::create(
        &fx.pool,
        fx.user_id,
        scooter.id,
        chrono::Duration::minutes(10),
    )
    .await
    .expect("booking");

    let (status, body) = post_json(
        &fx.state,
        "/api/v1/rides",
        &fx.token,
        serde_json::json!({ "scooter_id": scooter.id, "reservation_id": booking.id }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["reservation_id"], booking.id.to_string());
    let ride_id: Uuid = body["id"].as_str().expect("id").parse().unwrap();

    assert_eq!(
        db::bookings::find_by_id(&fx.pool, booking.id)
            .await
            .unwrap()
            .status,
        "converted"
    );
    assert_eq!(
        db::scooters::find_by_id(&fx.pool, scooter.id)
            .await
            .unwrap()
            .status,
        "rented"
    );

    // Самокат больше не стартует ни напрямую...
    let rival = setup_user(&fx.pool).await;
    let (status, body) = post_json(
        &fx.state,
        "/api/v1/rides",
        &bearer(rival),
        serde_json::json!({ "scooter_id": scooter.id }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(body["code"], "scooter_unavailable");

    // ...ни по той же брони (уже converted → 409, не 404).
    let (status, body) = post_json(
        &fx.state,
        "/api/v1/rides",
        &fx.token,
        serde_json::json!({ "scooter_id": scooter.id, "reservation_id": booking.id }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(body["code"], "reservation_expired");

    // Финиш закрывает цикл.
    let (status, _) = post_json(
        &fx.state,
        &format!("/api/v1/rides/{ride_id}/finish"),
        &fx.token,
        serde_json::json!({}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    teardown(&fx).await;
    let _ = db::bookings::delete_by_user_ids(&fx.pool, &[fx.user_id]).await;
}

/// Лимит: у юзера уже есть активная поездка → 409 ride_in_progress,
/// второй самокат остаётся available (rollback), после финиша можно снова.
#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn second_active_ride_is_409_and_rolls_back() {
    let mut fx = Fixture::setup().await;
    let first = fx.scooter().await;
    let second_scooter = fx.scooter().await;

    let (status, body) = post_json(
        &fx.state,
        "/api/v1/rides",
        &fx.token,
        serde_json::json!({ "scooter_id": first.id }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let outbox_before = db::outbox::recent(&fx.pool, 100).await.unwrap().len();
    let (status, body) = post_json(
        &fx.state,
        "/api/v1/rides",
        &fx.token,
        serde_json::json!({ "scooter_id": second_scooter.id }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(body["code"], "ride_in_progress");

    assert_eq!(
        db::scooters::find_by_id(&fx.pool, second_scooter.id)
            .await
            .unwrap()
            .status,
        "available"
    );
    let outbox_after = db::outbox::recent(&fx.pool, 100).await.unwrap().len();
    assert_eq!(outbox_before, outbox_after, "failed start must not outbox");

    // После финиша старт снова работает.
    let (status, page) = send(&fx.state, "GET", "/api/v1/rides", &fx.token, None).await;
    assert_eq!(status, StatusCode::OK, "body: {page}");
    let active: Uuid = page["items"][0]["id"]
        .as_str()
        .expect("ride id")
        .parse()
        .unwrap();
    let (status, _) = post_json(
        &fx.state,
        &format!("/api/v1/rides/{active}/finish"),
        &fx.token,
        serde_json::json!({}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = post_json(
        &fx.state,
        "/api/v1/rides",
        &fx.token,
        serde_json::json!({ "scooter_id": second_scooter.id }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    teardown(&fx).await;
}

/// Чужая поездка не видна: GET/finish → 404, состояние не меняется.
#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn foreign_ride_is_404() {
    let mut fx = Fixture::setup().await;
    let scooter = fx.scooter().await;
    let (status, body) = post_json(
        &fx.state,
        "/api/v1/rides",
        &fx.token,
        serde_json::json!({ "scooter_id": scooter.id }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let ride_id: Uuid = body["id"].as_str().expect("id").parse().unwrap();

    let outsider = bearer(Uuid::new_v4());
    let (status, _) = send(
        &fx.state,
        "GET",
        &format!("/api/v1/rides/{ride_id}"),
        &outsider,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = post_json(
        &fx.state,
        &format!("/api/v1/rides/{ride_id}/finish"),
        &outsider,
        serde_json::json!({}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        db::rentals::find_owned(&fx.pool, fx.user_id, ride_id)
            .await
            .unwrap()
            .status,
        "active"
    );

    teardown(&fx).await;
}

/// История: свежие сверху, курсорная пагинация по started_at.
#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn history_paginates_by_started_at_cursor() {
    let mut fx = Fixture::setup().await;
    let first = fx.scooter().await;
    let second_scooter = fx.scooter().await;

    for scooter in [first.id, second_scooter.id] {
        let (status, body) = post_json(
            &fx.state,
            "/api/v1/rides",
            &fx.token,
            serde_json::json!({ "scooter_id": scooter }).to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "body: {body}");
        let ride_id: Uuid = body["id"].as_str().expect("id").parse().unwrap();
        let (status, _) = post_json(
            &fx.state,
            &format!("/api/v1/rides/{ride_id}/finish"),
            &fx.token,
            serde_json::json!({}).to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    // limit=1 → самая свежая (вторая поездка) + курсор.
    let (status, page) = send(&fx.state, "GET", "/api/v1/rides?limit=1", &fx.token, None).await;
    assert_eq!(status, StatusCode::OK, "body: {page}");
    let items = page["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    let newer_scooter = items[0]["scooter_id"].as_str().unwrap();
    assert_eq!(newer_scooter, second_scooter.id.to_string());
    let cursor = page["next_before"].as_str().expect("next_before");

    // Следующая страница: только старая поездка, курсора больше нет.
    let (status, page) = send(
        &fx.state,
        "GET",
        &format!("/api/v1/rides?limit=1&before={cursor}"),
        &fx.token,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {page}");
    let items = page["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["scooter_id"], first.id.to_string());
    assert!(page["next_before"].is_null());

    teardown(&fx).await;
}

/// Outbox атомарен со стартом/финишем: rental.started/finished + статусы самоката.
#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn outbox_records_ride_lifecycle() {
    let mut fx = Fixture::setup().await;
    let scooter = fx.scooter().await;

    let (status, body) = post_json(
        &fx.state,
        "/api/v1/rides",
        &fx.token,
        serde_json::json!({ "scooter_id": scooter.id }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let ride_id: Uuid = body["id"].as_str().expect("id").parse().unwrap();

    let events = db::outbox::recent(&fx.pool, 20).await.unwrap();
    let started = events
        .iter()
        .find(|e| e.topic == "rental.started.v1" && e.payload["rental_id"] == ride_id.to_string())
        .expect("rental.started.v1 in outbox");
    assert_eq!(started.payload["scooter_id"], scooter.id.to_string());
    assert!(events.iter().any(|e| {
        e.topic == "scooter.status.v1"
            && e.payload["scooter_id"] == scooter.id.to_string()
            && e.payload["status"] == "rented"
    }));

    let (status, _) = post_json(
        &fx.state,
        &format!("/api/v1/rides/{ride_id}/finish"),
        &fx.token,
        serde_json::json!({}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let events = db::outbox::recent(&fx.pool, 20).await.unwrap();
    let finished = events
        .iter()
        .find(|e| e.topic == "rental.finished.v1" && e.payload["rental_id"] == ride_id.to_string())
        .expect("rental.finished.v1 in outbox");
    assert!(finished.payload["amount_kopeks"].as_i64().unwrap() > 0);
    assert!(events.iter().any(|e| {
        e.topic == "scooter.status.v1"
            && e.payload["scooter_id"] == scooter.id.to_string()
            && e.payload["status"] == "available"
    }));

    teardown(&fx).await;
}

#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn start_unknown_scooter_is_404() {
    let fx = Fixture::setup().await;
    let (status, body) = post_json(
        &fx.state,
        "/api/v1/rides",
        &fx.token,
        serde_json::json!({ "scooter_id": Uuid::new_v4() }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn finish_with_half_coordinates_is_400() {
    let mut fx = Fixture::setup().await;
    let scooter = fx.scooter().await;
    let (status, body) = post_json(
        &fx.state,
        "/api/v1/rides",
        &fx.token,
        serde_json::json!({ "scooter_id": scooter.id }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let ride_id: Uuid = body["id"].as_str().expect("id").parse().unwrap();

    let (status, _) = post_json(
        &fx.state,
        &format!("/api/v1/rides/{ride_id}/finish"),
        &fx.token,
        serde_json::json!({ "lat": 43.238 }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Поездка осталась активной — ретрай с полными координатами проходит.
    let (status, _) = post_json(
        &fx.state,
        &format!("/api/v1/rides/{ride_id}/finish"),
        &fx.token,
        serde_json::json!({ "lat": 43.238, "lon": 76.889 }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    teardown(&fx).await;
}

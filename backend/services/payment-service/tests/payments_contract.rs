//! Контракт payment-service (MVP #5): методы оплаты, история, вебхук YooKassa,
//! gRPC PaymentOrchestrator (холд → capture), джоб сверки.
//!
//! Прогон: `cargo test --workspace`. Тесты, требующие Postgres, помечены
//! `#[ignore]` — запускаются после `make up && make migrate`:
//! `cargo test -p payment-service -- --ignored`.
//!
//! Кейсы docs/mvp.md §2 #5 + DoD: идемпотентность `ride:{rental_id}` —
//! повторные capture / вебхук не дают двойного списания; capture при
//! недоступности эквайринга уходит в retry-очередь (ADR-0003/0014).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::auth::JwtState;
use db::create_pool_lazy;
use http_body_util::BodyExt;
use payment_service::dto::WebhookNotification;
use payment_service::services::payments::{self, CaptureResult};
use payment_service::services::yookassa::YooKassa;
use payment_service::{router, AppState};
use serde_json::Value;
use sqlx::PgPool;
use std::sync::Arc;
use tower::util::ServiceExt;
use uuid::Uuid;

const JWT_SECRET: &str = "test-secret";

fn test_state(yookassa: YooKassa) -> AppState {
    AppState {
        // Пул ленивый и заведомо недоступный: контракты без БД должны держаться.
        pool: create_pool_lazy("postgres://invalid:invalid@127.0.0.1:1/none").expect("lazy pool"),
        jwt: JwtState(Arc::new(JWT_SECRET.to_owned())),
        yookassa,
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
    token: Option<&str>,
    body: Option<String>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
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

// ── Без БД: авторизация и вебхук ────────────────────────────────────────────

#[tokio::test]
async fn payment_history_requires_token() {
    for token in [None, Some("garbage.token.here")] {
        let (status, _) = send(
            &test_state(YooKassa::Emulated),
            "GET",
            "/api/v1/payments",
            token,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn payment_methods_require_token() {
    for token in [None, Some("garbage.token.here")] {
        let (status, _) = send(
            &test_state(YooKassa::Emulated),
            "GET",
            "/api/v1/payments/methods",
            token,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = send(
            &test_state(YooKassa::Emulated),
            "POST",
            "/api/v1/payments/methods",
            token,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

/// Вебхук — без JWT (openapi security: []) и всегда 200 (даже мусор/неизвестный платёж).
#[tokio::test]
async fn webhook_is_public_and_always_200() {
    let state = test_state(YooKassa::Emulated);
    let (status, _) = send(
        &state,
        "POST",
        "/api/v1/payments/webhook",
        None,
        Some("not-json".to_owned()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let notification = serde_json::json!({
        "type": "notification",
        "event": "payment.succeeded",
        "object": { "id": "unknown-yk-id", "status": "succeeded", "paid": true }
    });
    let (status, _) = send(
        &state,
        "POST",
        "/api/v1/payments/webhook",
        None,
        Some(notification.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn emulated_methods_list_and_bind() {
    let state = test_state(YooKassa::Emulated);
    let token = bearer(Uuid::new_v4());

    let (status, list) = send(
        &state,
        "GET",
        "/api/v1/payments/methods",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list[0]["card_last4"], "4444");

    let (status, bound) = send(
        &state,
        "POST",
        "/api/v1/payments/methods",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(
        bound["confirmation_url"]
            .as_str()
            .unwrap_or_default()
            .starts_with("https://"),
        "body: {bound}"
    );
}

// ── Живой Postgres: `cargo test -p payment-service -- --ignored` ────────────

struct Fixture {
    pool: PgPool,
    user_id: Uuid,
    scooter_codes: Vec<String>,
}

impl Fixture {
    async fn setup() -> Self {
        let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL is not set");
        let pool = create_pool_lazy(&database_url).expect("pool");
        let telegram_id = (Uuid::new_v4().as_u128() % (i64::MAX as u128)) as i64;
        let user_id = db::users::upsert_by_telegram_id(&pool, telegram_id, None)
            .await
            .expect("seed user")
            .id;
        Self {
            pool,
            user_id,
            scooter_codes: Vec::new(),
        }
    }

    /// Поездка, на которую можно повесить платёж (FK payments.rental_id).
    /// На юзера одна активная поездка (`uq_active_ride_per_user`), поэтому
    /// перед следующей — закрыть предыдущую (`close_rental`).
    async fn rental(&mut self) -> db::rentals::Rental {
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
        db::rentals::create_direct(
            &self.pool,
            self.user_id,
            scooter.id,
            Uuid::new_v4(),
            &format!("hold:{}", Uuid::new_v4()),
        )
        .await
        .expect("seed rental")
    }

    /// Закрывает поездку (self-check: лимит активных снова свободен).
    async fn close_rental(&self, rental_id: Uuid) {
        db::rentals::finish(
            &self.pool,
            self.user_id,
            rental_id,
            db::rentals::FinishData {
                finished_at: chrono::Utc::now(),
                total_min: 1,
                amount_kopeks: 1_000,
                lat: None,
                lon: None,
            },
        )
        .await
        .expect("close rental");
    }

    async fn outbox_count(&self, topic: &str) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM outbox WHERE topic = $1")
            .bind(topic)
            .fetch_one(&self.pool)
            .await
            .expect("count outbox")
    }

    /// События конкретного платежа (параллельные тесты пишут в общий outbox).
    async fn outbox_count_for_payment(&self, topic: &str, payment_id: Uuid) -> i64 {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM outbox
             WHERE topic = $1 AND payload->>'payment_id' = $2",
        )
        .bind(topic)
        .bind(payment_id.to_string())
        .fetch_one(&self.pool)
        .await
        .expect("count outbox for payment")
    }
}

async fn teardown(fx: &Fixture) {
    let _ = db::payments::delete_by_user_ids(&fx.pool, &[fx.user_id]).await;
    let _ = db::rentals::delete_by_user_ids(&fx.pool, &[fx.user_id]).await;
    let _ = db::scooters::delete_by_codes(&fx.pool, &fx.scooter_codes).await;
}

fn state_with(pool: &PgPool, yookassa: YooKassa) -> AppState {
    AppState {
        pool: pool.clone(),
        jwt: JwtState(Arc::new(JWT_SECRET.to_owned())),
        yookassa,
    }
}

/// Холд: одна запись на поездку, повтор — та же запись (двойной холд невозможен).
#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn hold_is_idempotent_per_ride() {
    let mut fx = Fixture::setup().await;
    let state = state_with(&fx.pool, YooKassa::Emulated);
    let rental = fx.rental().await;

    let hold_amount = 2900 + 800 * 60;
    let first = payments::hold(&state, fx.user_id, rental.id, hold_amount)
        .await
        .expect("hold");
    let second = payments::hold(&state, fx.user_id, rental.id, hold_amount)
        .await
        .expect("hold again");

    assert_eq!(first.id, second.id, "same ride -> same payment");
    assert_eq!(first.status, "hold");
    assert_eq!(first.idempotency_key, format!("ride:{}", rental.id));
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM payments WHERE rental_id = $1")
        .bind(rental.id)
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(rows, 1, "no duplicate payment rows");
    teardown(&fx).await;
}

/// DoD: повторные capture не дают двойного списания и двойных событий.
#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn repeated_capture_never_double_charges() {
    let mut fx = Fixture::setup().await;
    let state = state_with(&fx.pool, YooKassa::Emulated);
    let rental = fx.rental().await;

    payments::hold(&state, fx.user_id, rental.id, 100_000)
        .await
        .expect("hold");
    let first_payment = db::payments::find_by_rental(&fx.pool, rental.id)
        .await
        .unwrap()
        .expect("payment");

    let events_before = fx
        .outbox_count_for_payment("payment.events.v1", first_payment.id)
        .await;
    let first = payments::capture(&state, rental.id, 9_700)
        .await
        .expect("capture");
    assert_eq!(first, CaptureResult::Captured);
    let second = payments::capture(&state, rental.id, 9_700)
        .await
        .expect("capture again");
    assert_eq!(second, CaptureResult::AlreadyCaptured);

    let events_after = fx
        .outbox_count_for_payment("payment.events.v1", first_payment.id)
        .await;
    assert_eq!(events_after - events_before, 1, "exactly one payment event");

    let payment = db::payments::find_by_rental(&fx.pool, rental.id)
        .await
        .unwrap()
        .expect("payment");
    assert_eq!(payment.status, "captured");
    assert_eq!(
        payment.amount_kopeks, 9_700,
        "final amount, not hold amount"
    );
    teardown(&fx).await;
}

/// Вебхук: succeeded подтверждает списание идемпотентно, canceled снимает холд.
#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn webhooks_apply_idempotently() {
    let mut fx = Fixture::setup().await;
    let state = state_with(&fx.pool, YooKassa::Emulated);
    let rental = fx.rental().await;
    payments::hold(&state, fx.user_id, rental.id, 50_000)
        .await
        .expect("hold");
    let payment = db::payments::find_by_rental(&fx.pool, rental.id)
        .await
        .unwrap()
        .expect("payment");

    let notification = |event: &str| WebhookNotification {
        event: event.to_owned(),
        object: payment_service::dto::WebhookObject {
            id: payment.yookassa_id.clone(),
            status: Some("succeeded".to_owned()),
            paid: Some(true),
            metadata: None,
        },
    };

    payments::apply_webhook(&state, &notification("payment.succeeded")).await;
    payments::apply_webhook(&state, &notification("payment.succeeded")).await;
    let after = db::payments::find_by_rental(&fx.pool, rental.id)
        .await
        .unwrap()
        .expect("payment");
    assert_eq!(after.status, "captured");
    assert_eq!(
        fx.outbox_count_for_payment("payment.events.v1", payment.id)
            .await,
        1,
        "single event"
    );

    // Отмена на другом холде: canceled → статус canceled, capture больше нельзя.
    fx.close_rental(rental.id).await;
    let rental2 = fx.rental().await;
    payments::hold(&state, fx.user_id, rental2.id, 50_000)
        .await
        .expect("hold 2");
    let payment2 = db::payments::find_by_rental(&fx.pool, rental2.id)
        .await
        .unwrap()
        .expect("payment 2");
    payments::apply_webhook(
        &state,
        &WebhookNotification {
            event: "payment.canceled".to_owned(),
            object: payment_service::dto::WebhookObject {
                id: payment2.yookassa_id.clone(),
                status: Some("canceled".to_owned()),
                paid: Some(false),
                metadata: None,
            },
        },
    )
    .await;
    let canceled = db::payments::find_by_rental(&fx.pool, rental2.id)
        .await
        .unwrap()
        .expect("payment 2");
    assert_eq!(canceled.status, "canceled");
    assert!(matches!(
        payments::capture(&state, rental2.id, 1_000).await,
        Err(common::AppError::Conflict {
            code: "capture_failed",
            ..
        })
    ));
    teardown(&fx).await;
}

/// История платежей: свежие сверху, фильтр по ride_id.
#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn history_lists_user_payments() {
    let mut fx = Fixture::setup().await;
    let state = state_with(&fx.pool, YooKassa::Emulated);
    let rental_a = fx.rental().await;
    payments::hold(&state, fx.user_id, rental_a.id, 10_000)
        .await
        .expect("hold a");
    fx.close_rental(rental_a.id).await;
    let rental_b = fx.rental().await;
    payments::hold(&state, fx.user_id, rental_b.id, 20_000)
        .await
        .expect("hold b");

    let all = payments::history(&state, fx.user_id, None, 20)
        .await
        .expect("history");
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|p| p.user_id == fx.user_id));

    let filtered = payments::history(&state, fx.user_id, Some(rental_a.id), 20)
        .await
        .expect("filtered");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].rental_id, Some(rental_a.id));
    teardown(&fx).await;
}

/// ADR-0003/0014: capture при недоступном эквайринге — queued_for_retry,
/// джоб сверки доводит до captured и закрывает retry-запись.
#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn failed_capture_is_reconciled() {
    let mut fx = Fixture::setup().await;
    let failing = state_with(&fx.pool, YooKassa::Failing);
    let working = state_with(&fx.pool, YooKassa::Emulated);
    let rental = fx.rental().await;

    // Холд в шлюзе Failing отклоняется — сначала холдим через рабочий.
    payments::hold(&working, fx.user_id, rental.id, 50_000)
        .await
        .expect("hold");
    let outcome = payments::capture(&failing, rental.id, 5_500)
        .await
        .expect("queued");
    assert_eq!(outcome, CaptureResult::QueuedForRetry);
    let retries = fx.outbox_count("capture.retry.v1").await;
    assert!(retries >= 1, "retry record queued");
    let still_hold = db::payments::find_by_rental(&fx.pool, rental.id)
        .await
        .unwrap()
        .expect("payment");
    assert_eq!(still_hold.status, "hold");

    // Эквайринг вернулся: джоб сверки доводит capture и закрывает очередь.
    let processed = payment_service::services::reconcile::reconcile_once(&working)
        .await
        .expect("reconcile");
    assert!(processed >= 1);
    let captured = db::payments::find_by_rental(&fx.pool, rental.id)
        .await
        .unwrap()
        .expect("payment");
    assert_eq!(captured.status, "captured");
    assert_eq!(captured.amount_kopeks, 5_500);
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox WHERE topic = 'capture.retry.v1' AND published_at IS NULL",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(pending, 0, "retry queue drained");
    teardown(&fx).await;
}

/// gRPC PaymentOrchestrator end-to-end: канал rental-service → payment-service.
#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn grpc_hold_capture_and_receipt() {
    use proto::scootly::payment::v1::payment_orchestrator_client::PaymentOrchestratorClient;
    use proto::scootly::payment::v1::payment_orchestrator_server::PaymentOrchestratorServer;

    let mut fx = Fixture::setup().await;
    let rental = fx.rental().await;

    // Случайный свободный порт (гонка минимальна, тестовый контур).
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = probe.local_addr().unwrap();
    drop(probe);

    let state = state_with(&fx.pool, YooKassa::Emulated);
    let server = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(PaymentOrchestratorServer::new(
                payment_service::grpc::PaymentOrchestratorImpl { state },
            ))
            .serve(addr)
            .await
            .unwrap();
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let channel = tonic::transport::Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect_lazy();
    let mut client = PaymentOrchestratorClient::new(channel);

    let hold = client
        .hold(proto::scootly::payment::v1::HoldRequest {
            user_id: fx.user_id.to_string(),
            rental_id: rental.id.to_string(),
            hold_amount_kopeks: 50_000,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(hold.status, "hold");

    let capture = client
        .capture(proto::scootly::payment::v1::CaptureRequest {
            rental_id: rental.id.to_string(),
            amount_kopeks: 4_300,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(capture.outcome, "captured");

    let receipt = client
        .rental_payment(proto::scootly::payment::v1::RentalPaymentRequest {
            rental_id: rental.id.to_string(),
        })
        .await
        .unwrap()
        .into_inner();
    let receipt = receipt.payment.expect("payment in receipt");
    assert_eq!(receipt.status, "captured");
    assert_eq!(receipt.amount_kopeks, 4_300);

    // Повторный capture по gRPC — already_captured (двойного списания нет).
    let repeat = client
        .capture(proto::scootly::payment::v1::CaptureRequest {
            rental_id: rental.id.to_string(),
            amount_kopeks: 4_300,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(repeat.outcome, "already_captured");

    server.abort();
    teardown(&fx).await;
}

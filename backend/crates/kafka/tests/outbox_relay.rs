//! Контракт Outbox-релея (MVP #8, ADR-0008): публичные топики публикуются и
//! помечаются `published_at`, внутренние retry-очереди payment-service не
//! трогаются, сбой публикации оставляет записи на следующий проход.
//!
//! Прогон: `cargo test -p kafka -- --ignored` после `make up && make migrate`.

use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Default)]
struct Recording {
    published: std::sync::Mutex<Vec<(String, Option<String>, Value)>>,
    fail_on_topic: Option<&'static str>,
}

impl kafka::relay::Publish for Recording {
    async fn publish(&self, topic: &str, key: Option<&str>, payload: &Value) -> anyhow::Result<()> {
        if Some(topic) == self.fail_on_topic {
            anyhow::bail!("kafka is down");
        }
        self.published.lock().unwrap().push((
            topic.to_owned(),
            key.map(str::to_owned),
            payload.clone(),
        ));
        Ok(())
    }
}

async fn pool() -> PgPool {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL is not set");
    db::create_pool_lazy(&database_url).expect("pool")
}

async fn push_outbox(pool: &PgPool, topic: &str, payload: &Value) -> Uuid {
    sqlx::query_scalar("INSERT INTO outbox (topic, payload) VALUES ($1, $2) RETURNING id")
        .bind(topic)
        .bind(payload)
        .fetch_one(pool)
        .await
        .expect("insert outbox")
}

async fn published_at(pool: &PgPool, id: Uuid) -> Option<chrono::DateTime<chrono::Utc>> {
    sqlx::query_scalar("SELECT published_at FROM outbox WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("fetch outbox")
}

async fn cleanup(pool: &PgPool, ids: &[Uuid]) {
    sqlx::query("DELETE FROM outbox WHERE id = ANY($1)")
        .bind(ids)
        .execute(pool)
        .await
        .expect("cleanup outbox");
}

/// Дренаж старых неопубликованных записей (общая dev-БД копит хвосты
/// контрактных прогонов, батч релея — первые 200 по created_at).
async fn drain(pool: &PgPool) {
    sqlx::query(
        "UPDATE outbox SET published_at = now() WHERE published_at IS NULL AND topic = ANY($1)",
    )
    .bind(kafka::relay::PUBLIC_TOPICS)
    .execute(pool)
    .await
    .expect("drain outbox");
}

#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn public_topics_reach_publisher_and_get_marked() {
    let pool = pool().await;
    drain(&pool).await;
    let rental_id = Uuid::new_v4();
    let started = push_outbox(
        &pool,
        "rental.started.v1",
        &json!({"rental_id": rental_id, "scooter_id": Uuid::new_v4(), "user_id": Uuid::new_v4()}),
    )
    .await;
    let status = push_outbox(
        &pool,
        "scooter.status.v1",
        &json!({"scooter_id": Uuid::new_v4(), "status": "available"}),
    )
    .await;
    // Внутренняя retry-очередь — не для Kafka.
    let retry = push_outbox(
        &pool,
        "capture.retry.v1",
        &json!({"payment_id": Uuid::new_v4()}),
    )
    .await;

    let recorder = Recording::default();
    kafka::relay::run_once(&pool, &recorder)
        .await
        .expect("relay tick");

    {
        let sent = recorder.published.lock().unwrap();
        assert!(
            sent.iter()
                .any(|(topic, key, payload)| topic == "rental.started.v1"
                    && key.as_deref() == Some(rental_id.to_string().as_str())
                    && payload["rental_id"] == json!(rental_id)),
            "rental.started.v1 must be published with entity key"
        );
        assert!(sent.iter().any(|(topic, ..)| topic == "scooter.status.v1"));
        assert!(
            !sent.iter().any(|(topic, ..)| topic == "capture.retry.v1"),
            "retry queues are internal, relay must skip them"
        );
    }

    assert!(published_at(&pool, started).await.is_some());
    assert!(published_at(&pool, status).await.is_some());
    assert!(
        published_at(&pool, retry).await.is_none(),
        "retry record stays for the reconcile job"
    );

    cleanup(&pool, &[started, status, retry]).await;
}

#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn publish_failure_keeps_record_unpublished() {
    let pool = pool().await;
    drain(&pool).await;
    let failing = push_outbox(
        &pool,
        "rental.finished.v1",
        &json!({"rental_id": Uuid::new_v4(), "scooter_id": Uuid::new_v4()}),
    )
    .await;

    let recorder = Recording {
        fail_on_topic: Some("rental.finished.v1"),
        ..Recording::default()
    };
    // Сбой доставки не роняет проход: запись остаётся на ретрай.
    kafka::relay::run_once(&pool, &recorder)
        .await
        .expect("relay tick tolerates publish failure");

    assert!(published_at(&pool, failing).await.is_none());
    assert!(
        !recorder
            .published
            .lock()
            .unwrap()
            .iter()
            .any(|(topic, ..)| topic == "rental.finished.v1"),
        "failed record must not be reported as published"
    );

    cleanup(&pool, &[failing]).await;
}

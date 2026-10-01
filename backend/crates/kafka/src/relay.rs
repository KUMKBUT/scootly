//! Outbox → Kafka релей (MVP #8, ADR-0008): бизнес-данные и запись outbox
//! committed одной транзакцией, релей публикует их в Kafka отдельным циклом.
//!
//! Контракт at-least-once: запись помечается `published_at` только после
//! подтверждения доставки; сбой оставляет её на следующий проход. Порядок
//! внутри сущности сохраняется: батч идёт по `created_at`, при сбое проход
//! прерывается, не перескакивая несостоявшуюся запись.

use std::time::Duration;

use sqlx::PgPool;

use crate::publisher::Publisher;

/// Публичные топики (docs/api/asyncapi.yaml): уходят в Kafka.
/// Внутренние retry-очереди payment-service (`capture.retry.v1`,
/// `void.retry.v1`) в списке нет — их разбирает джоб сверки.
pub const PUBLIC_TOPICS: &[&str] = &[
    "booking.created.v1",
    "booking.expired.v1",
    "rental.started.v1",
    "rental.unlock-failed.v1",
    "rental.finished.v1",
    "payment.events.v1",
    "scooter.status.v1",
];

/// Размер батча одного прохода.
pub const BATCH: i64 = 200;

/// Адресат публикации: чтобы не тянуть rdkafka в unit-тесты релея.
pub trait Publish: Send + Sync {
    fn publish(
        &self,
        topic: &str,
        key: Option<&str>,
        payload: &serde_json::Value,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;
}

impl Publish for Publisher {
    async fn publish(
        &self,
        topic: &str,
        key: Option<&str>,
        payload: &serde_json::Value,
    ) -> anyhow::Result<()> {
        Publisher::publish(self, topic, key, payload).await
    }
}

/// Ключ партиции — id сущности из payload. Приоритет `rental_id`: события
/// одной поездки (старт → финиш → capture) остаются в одной партиции.
/// Нет id → `None` (рандомная партиция).
pub fn message_key(payload: &serde_json::Value) -> Option<String> {
    const FIELDS: [&str; 4] = ["rental_id", "booking_id", "payment_id", "scooter_id"];
    FIELDS
        .iter()
        .find_map(|field| payload.get(*field).and_then(serde_json::Value::as_str))
        .map(str::to_owned)
}

/// Один проход: неопубликованные публичные записи → Kafka → `published_at`.
/// Возвращает число опубликованных; при сбое доставки останавливается,
/// чтобы не нарушить порядок.
pub async fn run_once(pool: &PgPool, publisher: &impl Publish) -> anyhow::Result<usize> {
    let batch = db::outbox::unpublished_any(pool, PUBLIC_TOPICS, BATCH).await?;
    let mut published = 0;
    for record in &batch {
        let key = message_key(&record.payload);
        match publisher
            .publish(&record.topic, key.as_deref(), &record.payload)
            .await
        {
            Ok(()) => {
                db::outbox::mark_published(pool, record.id).await?;
                published += 1;
            }
            Err(error) => {
                metrics::counter!("outbox_publish_failed_total").increment(1);
                tracing::warn!(
                    %error,
                    outbox_id = %record.id,
                    topic = %record.topic,
                    "kafka publish failed, will retry next tick"
                );
                break;
            }
        }
    }
    if published > 0 {
        metrics::counter!("outbox_published_total").increment(published as u64);
        tracing::debug!(published, "outbox relay batch done");
    }
    Ok(published)
}

/// Цикл релея; на shutdown добивает текущий батч и flush продюсера (ADR-0014).
pub async fn run(
    pool: PgPool,
    publisher: Publisher,
    interval: Duration,
    shutdown: impl std::future::Future<Output = ()>,
) {
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tokio::time::sleep(interval) => {
                if let Err(error) = run_once(&pool, &publisher).await {
                    tracing::warn!(%error, "outbox relay tick failed");
                }
            }
        }
    }
    if let Err(error) = run_once(&pool, &publisher).await {
        tracing::warn!(%error, "outbox relay final batch failed");
    }
    publisher.flush(Duration::from_secs(5)).ok();
    tracing::info!("outbox relay stopped");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn public_topics_cover_asyncapi_and_skip_retry_queues() {
        for topic in PUBLIC_TOPICS {
            assert!(
                topic.ends_with(".v1"),
                "topic {topic} must be versioned (ADR-0008)"
            );
            assert!(!topic.ends_with(".dlq"), "relay never writes to DLQ");
        }
        assert!(!PUBLIC_TOPICS.contains(&"capture.retry.v1"));
        assert!(!PUBLIC_TOPICS.contains(&"void.retry.v1"));
    }

    #[test]
    fn message_key_prefers_entity_id() {
        let ride = json!({"rental_id": "R", "scooter_id": "S"});
        assert_eq!(message_key(&ride).as_deref(), Some("R"));
        let booking = json!({"booking_id": "B", "user_id": "U"});
        assert_eq!(message_key(&booking).as_deref(), Some("B"));
        let payment = json!({"payment_id": "P", "rental_id": "R"});
        // Приоритет rental_id: порядок событий одной поездки сохраняется.
        assert_eq!(message_key(&payment).as_deref(), Some("R"));
        let scooter = json!({"scooter_id": "S", "status": "available"});
        assert_eq!(message_key(&scooter).as_deref(), Some("S"));
    }

    #[test]
    fn message_key_without_entity_id_is_none() {
        assert!(message_key(&json!({"reason": "ttl"})).is_none());
        assert!(message_key(&json!({})).is_none());
    }
}

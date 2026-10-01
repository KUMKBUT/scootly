//! Репозиторий `outbox` (ADR-0008): бизнес-данные + события — одна транзакция,
//! публикацию в Kafka делает отдельный воркер (MVP #8).

use common::{AppError, AppResult};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct OutboxRecord {
    pub id: Uuid,
    pub topic: String,
    pub payload: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Кладёт событие в outbox внутри текущей транзакции (`&mut *tx`).
pub async fn push(
    tx: &mut sqlx::PgConnection,
    topic: &str,
    payload: &serde_json::Value,
) -> AppResult<()> {
    sqlx::query("INSERT INTO outbox (topic, payload) VALUES ($1, $2)")
        .bind(topic)
        .bind(payload)
        .execute(tx)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
}

/// Вне бизнес-транзакции: служебные записи (retry-очередь capture, ADR-0003).
pub async fn push_pool(pool: &PgPool, topic: &str, payload: &serde_json::Value) -> AppResult<()> {
    sqlx::query("INSERT INTO outbox (topic, payload) VALUES ($1, $2)")
        .bind(topic)
        .bind(payload)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
}

/// Последние события (диагностика и тест-контракты).
pub async fn recent(pool: &PgPool, limit: i64) -> AppResult<Vec<OutboxRecord>> {
    sqlx::query_as::<_, OutboxRecord>(
        "SELECT id, topic, payload, created_at, published_at
         FROM outbox ORDER BY created_at DESC, id DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))
}

/// Неопубликованные записи топика (джоб сверки capture, ADR-0003; релей — MVP #8).
pub async fn unpublished_by_topic(
    pool: &PgPool,
    topic: &str,
    limit: i64,
) -> AppResult<Vec<OutboxRecord>> {
    sqlx::query_as::<_, OutboxRecord>(
        "SELECT id, topic, payload, created_at, published_at
         FROM outbox
         WHERE topic = $1 AND published_at IS NULL
         ORDER BY created_at
         LIMIT $2",
    )
    .bind(topic)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))
}

/// Помечает запись опубликованной (после успешной обработки).
pub async fn mark_published(pool: &PgPool, id: Uuid) -> AppResult<()> {
    sqlx::query("UPDATE outbox SET published_at = now() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
}

/// Обновляет payload (например, счётчик retry-попыток, ADR-0010).
pub async fn set_payload(pool: &PgPool, id: Uuid, payload: &serde_json::Value) -> AppResult<()> {
    sqlx::query("UPDATE outbox SET payload = $2 WHERE id = $1")
        .bind(id)
        .bind(payload)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
}

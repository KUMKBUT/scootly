//! Репозиторий `ride_attempts` (попытки unlock, MVP #6, ADR-0006).
//! Никаких прямых SQL-запросов вне crates/db.
//!
//! Попытка пишется ДО отправки unlock: `pending → acked` (замок подтвердил)
//! или `pending → failed` (таймаут 10 c — холд снимается, самокат offline).
//! Идемпотентность: разрешение попытки — `UPDATE .. WHERE status='pending'`,
//! повторный вызов состояние не меняет.

use common::{AppError, AppResult};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct RideAttempt {
    pub id: Uuid,
    pub rental_id: Uuid,
    pub scooter_id: Uuid,
    /// `pending | acked | failed` (ADR-0006).
    pub status: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub resolved_at: Option<chrono::DateTime<chrono::Utc>>,
}

const ATTEMPT_COLUMNS: &str = "id, rental_id, scooter_id, status, created_at, resolved_at";

fn internal(error: sqlx::Error) -> AppError {
    AppError::Internal(error.into())
}

/// Новая попытка unlock: строка `pending` до отправки команды замку.
pub async fn create_pending(
    pool: &PgPool,
    rental_id: Uuid,
    scooter_id: Uuid,
) -> AppResult<RideAttempt> {
    sqlx::query_as::<_, RideAttempt>(&format!(
        r#"
        INSERT INTO ride_attempts (rental_id, scooter_id)
        VALUES ($1, $2)
        RETURNING {ATTEMPT_COLUMNS}
        "#
    ))
    .bind(rental_id)
    .bind(scooter_id)
    .fetch_one(pool)
    .await
    .map_err(internal)
}

/// Замок подтвердил unlock: `pending → acked` (идемпотентно).
pub async fn mark_acked(pool: &PgPool, attempt_id: Uuid) -> AppResult<Option<RideAttempt>> {
    resolve(pool, attempt_id, "acked").await
}

/// Таймаут ack (10 c): `pending → failed` (идемпотентно).
pub async fn mark_failed(pool: &PgPool, attempt_id: Uuid) -> AppResult<Option<RideAttempt>> {
    resolve(pool, attempt_id, "failed").await
}

async fn resolve(
    pool: &PgPool,
    attempt_id: Uuid,
    to_status: &str,
) -> AppResult<Option<RideAttempt>> {
    sqlx::query_as::<_, RideAttempt>(&format!(
        r#"
        UPDATE ride_attempts
        SET status = $2, resolved_at = now()
        WHERE id = $1 AND status = 'pending'
        RETURNING {ATTEMPT_COLUMNS}
        "#
    ))
    .bind(attempt_id)
    .bind(to_status)
    .fetch_optional(pool)
    .await
    .map_err(internal)
}

/// Попытки поездки, свежие сверху (диагностика «самокат не открылся»).
pub async fn by_rental(pool: &PgPool, rental_id: Uuid) -> AppResult<Vec<RideAttempt>> {
    sqlx::query_as::<_, RideAttempt>(&format!(
        "SELECT {ATTEMPT_COLUMNS} FROM ride_attempts
         WHERE rental_id = $1 ORDER BY created_at DESC, id DESC"
    ))
    .bind(rental_id)
    .fetch_all(pool)
    .await
    .map_err(internal)
}

/// Чистка за собой в тестах (по FK на rentals юзеров).
pub async fn delete_by_rental_ids(pool: &PgPool, rental_ids: &[Uuid]) -> AppResult<u64> {
    let result = sqlx::query("DELETE FROM ride_attempts WHERE rental_id = ANY($1)")
        .bind(rental_ids)
        .execute(pool)
        .await
        .map_err(internal)?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_automaton_matches_adr0006() {
        let attempt = RideAttempt {
            id: Uuid::new_v4(),
            rental_id: Uuid::new_v4(),
            scooter_id: Uuid::new_v4(),
            status: "acked".to_owned(),
            created_at: chrono::Utc::now(),
            resolved_at: Some(chrono::Utc::now()),
        };
        assert_eq!(
            serde_json::to_value(&attempt).unwrap()["status"],
            "acked",
            "pending -> acked | failed only"
        );
    }
}

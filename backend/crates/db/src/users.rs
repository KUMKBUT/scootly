//! Репозиторий `users`. Никаких прямых SQL-запросов вне crates/db.

use common::{AppError, AppResult};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct User {
    pub id: Uuid,
    pub telegram_id: i64,
    pub phone: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Создаёт пользователя при первом входе, иначе обновляет телефон (если передан).
pub async fn upsert_by_telegram_id(
    pool: &PgPool,
    telegram_id: i64,
    phone: Option<&str>,
) -> AppResult<User> {
    sqlx::query_as::<_, User>(
        r#"
        INSERT INTO users (telegram_id, phone)
        VALUES ($1, $2)
        ON CONFLICT (telegram_id) DO UPDATE
            SET phone = COALESCE(EXCLUDED.phone, users.phone)
        RETURNING id, telegram_id, phone, created_at
        "#,
    )
    .bind(telegram_id)
    .bind(phone)
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))
}

pub async fn find_by_id(pool: &PgPool, id: Uuid) -> AppResult<User> {
    sqlx::query_as::<_, User>("SELECT id, telegram_id, phone, created_at FROM users WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .map_err(|e| match e {
            sqlx::Error::RowNotFound => AppError::NotFound(format!("user {id}")),
            other => AppError::Internal(other.into()),
        })
}

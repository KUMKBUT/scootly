//! Репозитории и пул подключений PostgreSQL.
//! Никаких прямых SQL-запросов вне этого крейта.

pub mod bookings;
pub mod outbox;
pub mod payments;
pub mod rentals;
pub mod scooters;
pub mod users;

use sqlx::postgres::{PgPool, PgPoolOptions};

pub async fn create_pool(database_url: &str) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await?;
    Ok(pool)
}

/// Ленивый пул без коннекта на старте (тесты, воркеры). Небольшой
/// acquire-timeout: тесты с заведомо недоступной БД падают быстро,
/// а не через дефолтные 30 c ожидания коннекта.
pub fn create_pool_lazy(database_url: &str) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(std::time::Duration::from_secs(2))
        .connect_lazy(database_url)?;
    Ok(pool)
}

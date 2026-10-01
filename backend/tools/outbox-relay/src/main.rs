//! Outbox → Kafka релей (MVP #8, ADR-0008): неопубликованные записи таблицы
//! `outbox` публикуются в Kafka, после подтверждения доставки помечаются
//! `published_at`. Отдельный воркер: события всех сервисов (rental, payment)
//! идут через общую таблицу outbox. Env: `DATABASE_URL`, `KAFKA_BROKERS`,
//! `OUTBOX_RELAY_INTERVAL_MS`.

use anyhow::Context;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is not set")?;
    let brokers = kafka::brokers_from_env();
    let interval = Duration::from_millis(
        std::env::var("OUTBOX_RELAY_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000),
    );

    let pool = db::create_pool(&database_url).await?;
    sqlx::migrate!("../../crates/db/migrations")
        .run(&pool)
        .await?;
    let publisher = kafka::Publisher::new(&brokers)?;

    tracing::info!(brokers = %brokers, ?interval, "outbox-relay started");
    kafka::relay::run(pool, publisher, interval, shutdown_signal()).await;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received");
}

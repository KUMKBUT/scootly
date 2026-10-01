//! Outbox → Kafka релей (MVP #8, ADR-0008): неопубликованные записи таблицы
//! `outbox` публикуются в Kafka, после подтверждения доставки помечаются
//! `published_at`. Отдельный воркер: события всех сервисов (rental, payment)
//! идут через общую таблицу outbox. Env: `DATABASE_URL`, `KAFKA_BROKERS`,
//! `OUTBOX_RELAY_INTERVAL_MS`.

use anyhow::Context;
use axum::{routing::get, Router};
use std::net::SocketAddr;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    common::metrics::install();

    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is not set")?;
    let brokers = kafka::brokers_from_env();
    let interval = Duration::from_millis(
        std::env::var("OUTBOX_RELAY_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000),
    );
    // Метрики релея (outbox_published_total) Prometheus собирает по HTTP —
    // тот же :9000, что у сервисов (infra/monitoring/prometheus/prometheus.yml).
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(9000);

    let pool = db::create_pool(&database_url).await?;
    sqlx::migrate!("../../crates/db/migrations")
        .run(&pool)
        .await?;
    let publisher = kafka::Publisher::new(&brokers)?;

    // HTTP для scrape — фоновым таском, релей завершается по signals.
    let metrics_addr = SocketAddr::from(([0, 0, 0, 0], port));
    let metrics_app =
        common::metrics::route(Router::new().route("/health", get(|| async { "ok" })));
    let metrics_server = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(metrics_addr)
            .await
            .expect("metrics port bind");
        if let Err(error) = axum::serve(listener, metrics_app).await {
            tracing::warn!(%error, "metrics server stopped");
        }
    });

    tracing::info!(brokers = %brokers, ?interval, "outbox-relay started");
    kafka::relay::run(pool, publisher, interval, shutdown_signal()).await;
    metrics_server.abort();
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received");
}

use anyhow::Context;
use common::auth::JwtState;
use rental_service::services::{locks::Locks, payments::Payments, tariff::Tariff};
use rental_service::{router, AppState, SWEEP_INTERVAL_SECS};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    common::metrics::install();

    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is not set")?;
    let jwt_secret = std::env::var("JWT_SECRET").context("JWT_SECRET is not set")?;
    // Redis нужен только для TTL-триггера: без него сервис деградирует, а не падает
    // (PG — source of truth, ADR-0003), поэтому URL не обязателен.
    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_owned());
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(9000);
    let sweep_interval: u64 = std::env::var("BOOKING_SWEEP_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(SWEEP_INTERVAL_SECS);
    // Тариф per_minute (docs/mvp.md §2): копейки, значения в env.
    let tariff = Tariff::from_env();
    // Шлюз оплаты (MVP #5): PAYMENT_SERVICE_URL → gRPC payment-service,
    // без переменной — эмуляция (локальный стенд без эквайринга).
    let payments = Payments::from_env();

    let pool = db::create_pool(&database_url).await?;
    sqlx::migrate!("../../crates/db/migrations")
        .run(&pool)
        .await?;

    let state = AppState {
        pool,
        jwt: JwtState(Arc::new(jwt_secret)),
        redis: redis_client::LazyConnection::new(&redis_url)?,
        tariff,
        locks: Locks::Emulated,
        payments,
    };

    // Джоб сверки броней (ADR-0003): TTL-автоснятие работает и без Redis.
    tokio::spawn(rental_service::services::reservations::run_sweeper(
        state.clone(),
        Duration::from_secs(sweep_interval),
    ));

    let app =
        common::metrics::route(router(state).layer(tower_http::trace::TraceLayer::new_for_http()));

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!(
        unlock_kopeks = tariff.unlock_kopeks,
        per_min_kopeks = tariff.per_min_kopeks,
        "rental-service listening on {}",
        addr
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received");
}

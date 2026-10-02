use anyhow::Context;
use common::auth::JwtState;
use payment_service::services::yookassa::YooKassa;
use payment_service::{router, AppState, RECONCILE_INTERVAL_SECS};
use proto::scootly::payment::v1::payment_orchestrator_server::PaymentOrchestratorServer;
use std::net::SocketAddr;
use std::time::Duration;

use payment_service::grpc::PaymentOrchestratorImpl;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    common::metrics::install();

    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is not set")?;
    let jwt_secret = std::env::var("JWT_SECRET").context("JWT_SECRET is not set")?;
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(9000);
    let grpc_port: u16 = std::env::var("GRPC_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(9001);
    // Джоб сверки (ADR-0003): раз в 5 минут по умолчанию.
    let reconcile_secs: u64 = std::env::var("RECONCILIATION_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(RECONCILE_INTERVAL_SECS);

    let pool = db::create_pool(&database_url).await?;
    sqlx::migrate!("../../crates/db/migrations")
        .run(&pool)
        .await?;

    // Шлюз YooKassa (MVP #11): заданы YOOKASSA_SHOP_ID/YOOKASSA_SECRET_KEY —
    // настоящий API эквайринга, иначе эмуляция «на столе» (секреты — только
    // env, ADR-0014).
    let state = AppState {
        pool: pool.clone(),
        jwt: JwtState(std::sync::Arc::new(jwt_secret)),
        yookassa: YooKassa::from_env(),
    };

    // gRPC PaymentOrchestrator (rental-service) — отдельным таском,
    // HTTP API + вебхук — на своём порту. Оба с graceful shutdown (ADR-0014).
    let grpc_state = state.clone();
    let grpc_addr = SocketAddr::from(([0, 0, 0, 0], grpc_port));
    let grpc = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(PaymentOrchestratorServer::new(PaymentOrchestratorImpl {
                state: grpc_state,
            }))
            .serve_with_shutdown(grpc_addr, shutdown_signal("gRPC"))
            .await
    });

    // Джоб сверки: доводит capture, застрявшие из-за эквайринга (ADR-0003).
    tokio::spawn(payment_service::services::reconcile::run(
        state.clone(),
        Duration::from_secs(reconcile_secs),
    ));

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("payment-service http on {} (gRPC on {})", addr, grpc_port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        common::metrics::route(router(state).layer(tower_http::trace::TraceLayer::new_for_http())),
    )
    .with_graceful_shutdown(shutdown_signal("http"))
    .await?;

    grpc.abort();
    Ok(())
}

async fn shutdown_signal(what: &str) {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received ({what})");
}

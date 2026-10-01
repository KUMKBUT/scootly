use anyhow::Context;
use scooter_service::grpc::ScooterPositionsImpl;
use std::net::SocketAddr;

use proto::scootly::scooter::v1::scooter_positions_server::ScooterPositionsServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    common::metrics::install();

    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is not set")?;
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(9000);
    let grpc_port: u16 = std::env::var("GRPC_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(9001);

    let pool = db::create_pool(&database_url).await?;
    sqlx::migrate!("../../crates/db/migrations")
        .run(&pool)
        .await?;

    // gRPC last_position (ADR-0011 fallback) — отдельным таском,
    // HTTP /health — на своём порту. Оба с graceful shutdown.
    let grpc_pool = pool.clone();
    let grpc_addr = SocketAddr::from(([0, 0, 0, 0], grpc_port));
    let grpc = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(ScooterPositionsServer::new(ScooterPositionsImpl {
                pool: grpc_pool,
            }))
            .serve_with_shutdown(grpc_addr, shutdown_signal("gRPC"))
            .await
    });

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("scooter-service http on {} (gRPC on {})", addr, grpc_port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        common::metrics::route(scooter_service::http_router()),
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

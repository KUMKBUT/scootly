use anyhow::Context;
use common::auth::JwtState;
use geo_service::{router, AppState};
use proto::scootly::scooter::v1::scooter_positions_client::ScooterPositionsClient;
use redis_client::LazyConnection;
use std::net::SocketAddr;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let redis_url = std::env::var("REDIS_URL").context("REDIS_URL is not set")?;
    let jwt_secret = std::env::var("JWT_SECRET").context("JWT_SECRET is not set")?;
    // ADR-0011: fallback last_position по gRPC у scooter-service.
    let scooter_service_url =
        std::env::var("SCOOTER_SERVICE_URL").context("SCOOTER_SERVICE_URL is not set")?;
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(9000);

    // Lazy: и Redis, и gRPC-канал не коннектятся на старте (деградация — на запросе).
    let redis = LazyConnection::new(&redis_url)?;
    let channel = tonic::transport::Channel::from_shared(scooter_service_url)
        .context("SCOOTER_SERVICE_URL is not a valid uri")?
        .connect_lazy();
    let scooters = ScooterPositionsClient::new(channel);

    let state = AppState {
        jwt: JwtState(Arc::new(jwt_secret)),
        redis,
        scooters,
    };
    let app = router(state).layer(tower_http::trace::TraceLayer::new_for_http());

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("geo-service listening on {}", addr);
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

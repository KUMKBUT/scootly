use axum::{routing::get, Router};
use std::net::SocketAddr;

async fn health() -> &'static str {
    "ok"
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    common::metrics::install();

    let app = common::metrics::route(Router::new().route("/health", get(health)));

    let addr = SocketAddr::from(([0, 0, 0, 0], 9000));
    tracing::info!("bot-service listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

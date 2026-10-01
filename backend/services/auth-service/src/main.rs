use anyhow::Context;
use common::auth::JwtState;
use std::net::SocketAddr;
use std::sync::Arc;

use auth_service::{router, AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    common::metrics::install();

    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is not set")?;
    let jwt_secret = std::env::var("JWT_SECRET").context("JWT_SECRET is not set")?;
    let bot_token = std::env::var("TELEGRAM_BOT_TOKEN").context("TELEGRAM_BOT_TOKEN is not set")?;
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(9000);

    let pool = db::create_pool(&database_url).await?;
    sqlx::migrate!("../../crates/db/migrations")
        .run(&pool)
        .await?;

    let state = AppState {
        pool,
        jwt: JwtState(Arc::new(jwt_secret)),
        bot_token,
    };
    let app =
        common::metrics::route(router(state).layer(tower_http::trace::TraceLayer::new_for_http()));

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("auth-service listening on {}", addr);
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

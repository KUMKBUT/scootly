use anyhow::Context;
use common::auth::JwtState;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use ws_gateway::handlers::IDLE_TIMEOUT;
use ws_gateway::registry::Registry;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let redis_url = std::env::var("REDIS_URL").context("REDIS_URL is not set")?;
    let jwt_secret = std::env::var("JWT_SECRET").context("JWT_SECRET is not set")?;
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(9000);

    let sessions = Arc::new(Registry::new());

    // Fan-out карты: Redis pub/sub → подписанные сессии (websocket.md §7).
    // Kafka → Redis pub/sub мост приходит с outbox-воркером (MVP #8).
    let fanout_registry = sessions.clone();
    let fanout_url = redis_url.clone();
    let fanout = tokio::spawn(async move {
        ws_gateway::fanout::run(fanout_url, fanout_registry).await;
    });

    // Heartbeat: тишина 60 c → close (websocket.md §1).
    let reaper_registry = sessions.clone();
    let reaper = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(15));
        loop {
            ticker.tick().await;
            reaper_registry.kick_idle(IDLE_TIMEOUT);
        }
    });

    let state = ws_gateway::AppState {
        jwt: JwtState(Arc::new(jwt_secret)),
        sessions,
    };
    let app = ws_gateway::router(state).layer(tower_http::trace::TraceLayer::new_for_http());

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("ws-gateway listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    fanout.abort();
    reaper.abort();
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received");
}

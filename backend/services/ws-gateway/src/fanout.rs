//! Redis pub/sub → сессии. Источник — мост Kafka `scooter.status.v1` →
//! `ws:scooters` (приходит с outbox-воркером, MVP #8); формат — готовый
//! конверт `scooter.updated` / `scooter.removed` (websocket.md §4).

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;

use crate::registry::Registry;
use redis_client::geo::WS_SCOOTERS_CHANNEL;

const RECONNECT_DELAY: Duration = Duration::from_secs(1);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

pub async fn run(redis_url: String, registry: Arc<Registry>) {
    loop {
        match connect(&redis_url).await {
            Ok(mut pubsub) => {
                tracing::info!(channel = WS_SCOOTERS_CHANNEL, "subscribed to redis pub/sub");
                let mut stream = pubsub.on_message();
                while let Some(message) = stream.next().await {
                    let raw: String = match message.get_payload() {
                        Ok(raw) => raw,
                        Err(error) => {
                            tracing::warn!(%error, "bad pub/sub payload");
                            continue;
                        }
                    };
                    dispatch(&registry, &raw);
                }
                tracing::warn!("redis pub/sub stream ended");
            }
            Err(error) => tracing::warn!(%error, "redis pub/sub connect failed"),
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

async fn connect(redis_url: &str) -> anyhow::Result<redis::aio::PubSub> {
    let client = redis::Client::open(redis_url)?;
    tokio::time::timeout(CONNECT_TIMEOUT, async {
        let mut pubsub = client.get_async_pubsub().await?;
        pubsub.subscribe(WS_SCOOTERS_CHANNEL).await?;
        anyhow::Ok(pubsub)
    })
    .await
    .map_err(|_| anyhow::anyhow!("pub/sub connect timed out"))?
}

/// Роутит один конверт: `scooter.updated` — по гео-подпискам,
/// `scooter.removed` — всем подписанным на карту.
fn dispatch(registry: &Registry, raw: &str) {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        tracing::warn!("non-json pub/sub message, skipping");
        return;
    };
    match value.get("type").and_then(Value::as_str) {
        Some("scooter.updated") => {
            let Some(payload) = value.get("payload") else {
                tracing::warn!("scooter.updated without payload");
                return;
            };
            let (Some(lat), Some(lon)) = (
                payload.get("lat").and_then(Value::as_f64),
                payload.get("lon").and_then(Value::as_f64),
            ) else {
                tracing::warn!("scooter.updated without coordinates");
                return;
            };
            registry.broadcast_updated(lat, lon, raw.to_owned());
        }
        Some("scooter.removed") => registry.broadcast_removed(raw.to_owned()),
        _ => {
            // События ride.*/payment.* идут в приватные сессии (MVP #3+).
            tracing::debug!("ignoring non-map event");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{envelope, Subscription};
    use tokio::sync::mpsc;

    fn subscribed_registry() -> (
        Registry,
        mpsc::UnboundedReceiver<axum::extract::ws::Message>,
    ) {
        let registry = Registry::new();
        let (tx, rx) = mpsc::unbounded_channel();
        let user = uuid::Uuid::new_v4();
        registry.register(user, tx);
        registry.set_subscription(
            user,
            Some(Subscription {
                lat: 43.238,
                lon: 76.889,
                radius_m: 500.0,
            }),
        );
        (registry, rx)
    }

    #[test]
    fn dispatch_routes_updated_by_geo() {
        let (registry, mut rx) = subscribed_registry();

        let text = envelope(
            "scooter.updated",
            serde_json::json!({ "id": uuid::Uuid::new_v4(), "lat": 43.2382, "lon": 76.8892, "status": "available", "battery_pct": 80 }),
        );
        dispatch(&registry, &text);
        assert!(rx.try_recv().is_ok());

        let far = envelope(
            "scooter.updated",
            serde_json::json!({ "id": uuid::Uuid::new_v4(), "lat": 1.0, "lon": 1.0, "status": "available", "battery_pct": 80 }),
        );
        dispatch(&registry, &far);
        assert!(rx.try_recv().is_err(), "far event must be filtered out");
    }

    #[test]
    fn dispatch_removed_reaches_subscriber() {
        let (registry, mut rx) = subscribed_registry();
        dispatch(
            &registry,
            r#"{"type":"scooter.removed","payload":{"id":"5a1e...","reason":"offline"}}"#,
        );
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn dispatch_ignores_garbage_and_other_events() {
        let (registry, mut rx) = subscribed_registry();
        dispatch(&registry, "not json");
        dispatch(&registry, r#"{"type":"ride.started","payload":{}}"#);
        dispatch(
            &registry,
            r#"{"type":"scooter.updated","payload":{"status":"available"}}"#,
        );
        assert!(rx.try_recv().is_err());
    }
}

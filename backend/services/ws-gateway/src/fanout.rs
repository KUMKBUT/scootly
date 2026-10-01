//! Redis pub/sub → сессии. Источники — мосты Kafka → Redis (MVP #8,
//! websocket.md §7): карта `scooter.status.v1` → `ws:scooters` (конверты
//! `scooter.updated` / `scooter.removed`, §4), приватные события юзера →
//! `ws:users` (`{"user_id", "event"}` — доставляются только его сессии).

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;

use crate::registry::Registry;
use redis_client::geo::{WS_SCOOTERS_CHANNEL, WS_USERS_CHANNEL};

const RECONNECT_DELAY: Duration = Duration::from_secs(1);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

pub async fn run(redis_url: String, registry: Arc<Registry>) {
    loop {
        match connect(&redis_url).await {
            Ok(mut pubsub) => {
                tracing::info!(
                    channel = WS_SCOOTERS_CHANNEL,
                    channel2 = WS_USERS_CHANNEL,
                    "subscribed to redis pub/sub"
                );
                let mut stream = pubsub.on_message();
                while let Some(message) = stream.next().await {
                    let raw: String = match message.get_payload() {
                        Ok(raw) => raw,
                        Err(error) => {
                            tracing::warn!(%error, "bad pub/sub payload");
                            continue;
                        }
                    };
                    dispatch(&registry, message.get_channel_name(), &raw);
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
        pubsub.subscribe(WS_USERS_CHANNEL).await?;
        anyhow::Ok(pubsub)
    })
    .await
    .map_err(|_| anyhow::anyhow!("pub/sub connect timed out"))?
}

/// Роутит один конверт по каналу-источнику:
/// `ws:scooters` — карта (гео-фильтр подписок), `ws:users` — приватные события.
fn dispatch(registry: &Registry, channel: &str, raw: &str) {
    if channel == WS_USERS_CHANNEL {
        route_user_event(registry, raw);
        return;
    }
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
            tracing::debug!("ignoring non-map event");
        }
    }
}

/// `{"user_id": "...", "event": {...конверт...}}` → сессии юзера (MVP #8).
fn route_user_event(registry: &Registry, raw: &str) {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        tracing::warn!("non-json pub/sub message on {WS_USERS_CHANNEL}");
        return;
    };
    let Some(user_id) = value
        .get("user_id")
        .and_then(Value::as_str)
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
    else {
        tracing::warn!("user event without user_id, skipping");
        return;
    };
    let Some(event) = value.get("event") else {
        tracing::warn!("user event without event envelope, skipping");
        return;
    };
    registry.send_to_user(user_id, event.to_string());
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
        dispatch(&registry, WS_SCOOTERS_CHANNEL, &text);
        assert!(rx.try_recv().is_ok());

        let far = envelope(
            "scooter.updated",
            serde_json::json!({ "id": uuid::Uuid::new_v4(), "lat": 1.0, "lon": 1.0, "status": "available", "battery_pct": 80 }),
        );
        dispatch(&registry, WS_SCOOTERS_CHANNEL, &far);
        assert!(rx.try_recv().is_err(), "far event must be filtered out");
    }

    #[test]
    fn dispatch_removed_reaches_subscriber() {
        let (registry, mut rx) = subscribed_registry();
        dispatch(
            &registry,
            WS_SCOOTERS_CHANNEL,
            r#"{"type":"scooter.removed","payload":{"id":"5a1e...","reason":"offline"}}"#,
        );
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn dispatch_ignores_garbage_and_other_events() {
        let (registry, mut rx) = subscribed_registry();
        dispatch(&registry, WS_SCOOTERS_CHANNEL, "not json");
        dispatch(
            &registry,
            WS_SCOOTERS_CHANNEL,
            r#"{"type":"ride.started","payload":{}}"#,
        );
        dispatch(
            &registry,
            WS_SCOOTERS_CHANNEL,
            r#"{"type":"scooter.updated","payload":{"status":"available"}}"#,
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn dispatch_delivers_user_event_only_to_owner() {
        let (registry, mut rx) = subscribed_registry();
        let user = uuid::Uuid::new_v4();
        let (tx, mut rx_owner) = mpsc::unbounded_channel();
        registry.register(user, tx);

        let message = serde_json::json!({
            "user_id": user,
            "event": crate::protocol::envelope_value(
                "ride.started",
                serde_json::json!({ "ride_id": uuid::Uuid::new_v4(), "scooter_id": uuid::Uuid::new_v4(), "started_at": "2026-10-01T12:00:00Z" }),
            ),
        });
        dispatch(&registry, WS_USERS_CHANNEL, &message.to_string());

        let text = match rx_owner.try_recv().unwrap() {
            axum::extract::ws::Message::Text(text) => text,
            other => panic!("expected text, got {other:?}"),
        };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["type"], "ride.started");

        // Чужая сессия (с картой или без) ничего не получает.
        assert!(
            rx.try_recv().is_err(),
            "other user must not see private event"
        );
    }

    #[test]
    fn dispatch_user_event_without_session_is_dropped() {
        let (registry, _rx) = subscribed_registry();
        let message = serde_json::json!({
            "user_id": uuid::Uuid::new_v4(),
            "event": crate::protocol::envelope_value("ride.finished", serde_json::json!({ "ride_id": uuid::Uuid::new_v4() })),
        });
        // Не падает: нет сессии — событие некуда доставлять (websocket.md §6).
        dispatch(&registry, WS_USERS_CHANNEL, &message.to_string());
    }

    #[test]
    fn dispatch_rejects_malformed_user_events() {
        let (registry, mut rx) = subscribed_registry();
        dispatch(&registry, WS_USERS_CHANNEL, "not json");
        dispatch(
            &registry,
            WS_USERS_CHANNEL,
            r#"{"event": {"type": "ride.started"}}"#,
        );
        dispatch(
            &registry,
            WS_USERS_CHANNEL,
            r#"{"user_id": "not-a-uuid", "event": {}}"#,
        );
        assert!(rx.try_recv().is_err());
    }
}

//! Мост Kafka → Redis pub/sub (MVP #8, websocket.md §7): доменные события
//! `*.v1` (docs/api/asyncapi.yaml) превращаются в WS-конверты и публикуются
//! в каналы фан-аута шлюза: карта — `ws:scooters`, приватные события юзера —
//! `ws:users`. Шлюз ничего не пишет в PG: снапшоты — REST (§6).

use rdkafka::config::ClientConfig;
use rdkafka::consumer::{Consumer, StreamConsumer};
use rdkafka::Message;
use serde_json::{json, Value};
use uuid::Uuid;

use redis_client::geo::{WS_SCOOTERS_CHANNEL, WS_USERS_CHANNEL};

/// Топики, события которых доходят до приложения (asyncapi + websocket.md §4).
pub const TOPICS: &[&str] = &[
    "booking.created.v1",
    "booking.expired.v1",
    "rental.started.v1",
    "rental.unlock-failed.v1",
    "rental.finished.v1",
    "payment.events.v1",
    "scooter.status.v1",
];

const RECONNECT_DELAY: std::time::Duration = std::time::Duration::from_secs(1);
/// Один consumer-group на все поды шлюза: событие читается один раз,
/// дальше раздаётся через Redis pub/sub (websocket.md §7).
const GROUP_ID: &str = "ws-gateway";

pub async fn run(brokers: String, redis_url: String) {
    loop {
        match connect(&brokers) {
            Ok(consumer) => match redis::Client::open(redis_url.as_str()) {
                Ok(client) => match client.get_connection_manager().await {
                    Ok(mut conn) => {
                        tracing::info!(topics = ?TOPICS, "kafka bridge subscribed");
                        consume(&consumer, &mut conn).await;
                        tracing::warn!("kafka consumer stream ended");
                    }
                    Err(error) => tracing::warn!(%error, "redis connect failed (bridge)"),
                },
                Err(error) => tracing::error!(%error, "bad redis url (bridge)"),
            },
            Err(error) => tracing::warn!(%error, "kafka consumer connect failed"),
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

fn connect(brokers: &str) -> anyhow::Result<StreamConsumer> {
    let consumer: StreamConsumer = ClientConfig::new()
        .set("bootstrap.servers", brokers)
        .set("group.id", GROUP_ID)
        // Живые обновления: накопленная история не нужна — снапшоты берёт REST.
        .set("auto.offset.reset", "latest")
        .set("enable.auto.commit", "true")
        .create()?;
    consumer.subscribe(TOPICS)?;
    Ok(consumer)
}

async fn consume(consumer: &StreamConsumer, conn: &mut redis::aio::ConnectionManager) {
    loop {
        match consumer.recv().await {
            Ok(message) => {
                let topic = message.topic().to_owned();
                let raw = message.payload().map(<[u8]>::to_vec);
                if let Err(error) = handle(conn, &topic, raw.as_deref()).await {
                    tracing::warn!(%error, topic, "kafka event handling failed");
                }
            }
            Err(error) => {
                tracing::warn!(%error, "kafka consumer error");
                return;
            }
        }
    }
}

async fn handle(
    conn: &mut redis::aio::ConnectionManager,
    topic: &str,
    raw: Option<&[u8]>,
) -> anyhow::Result<()> {
    let Some(raw) = raw else {
        return Ok(());
    };
    let Ok(payload) = serde_json::from_slice::<Value>(raw) else {
        tracing::warn!(topic, "non-json kafka payload, skipping");
        return Ok(());
    };

    if topic == "scooter.status.v1" {
        let (kind, event) = map_scooter_status(conn, &payload).await;
        let Some((kind, event)) = kind.zip(event) else {
            tracing::debug!(topic, "scooter status without map data, skipping");
            return Ok(());
        };
        publish(
            conn,
            WS_SCOOTERS_CHANNEL,
            &crate::protocol::envelope(kind, event),
        )
        .await?;
        return Ok(());
    }

    let Some((user_id, kind, event)) = map_user_event(topic, &payload) else {
        tracing::debug!(topic, "event without app mapping, skipping");
        return Ok(());
    };
    let message = json!({
        "user_id": user_id,
        "event": crate::protocol::envelope_value(kind, event),
    });
    publish(conn, WS_USERS_CHANNEL, &message.to_string()).await
}

async fn publish(
    conn: &mut redis::aio::ConnectionManager,
    channel: &str,
    text: &str,
) -> anyhow::Result<()> {
    let receivers: i64 = redis::cmd("PUBLISH")
        .arg(channel)
        .arg(text)
        .query_async(conn)
        .await?;
    tracing::debug!(channel, receivers, "ws event published");
    Ok(())
}

/// `scooter.status.v1` → конверты карты: `offline` → `scooter.removed`
/// (координаты не нужны), остальное → `scooter.updated` по geo-хэшу
/// (нет хэша — самоката нет на карте, обновлять нечего).
async fn map_scooter_status(
    conn: &mut redis::aio::ConnectionManager,
    payload: &Value,
) -> (Option<&'static str>, Option<Value>) {
    let Some(id) = payload.get("scooter_id").and_then(Value::as_str) else {
        return (None, None);
    };
    let Some(status) = payload.get("status").and_then(Value::as_str) else {
        return (None, None);
    };
    if status == "offline" {
        return (
            Some("scooter.removed"),
            Some(json!({ "id": id, "reason": "offline" })),
        );
    }
    let Ok(scooter_id) = Uuid::parse_str(id) else {
        return (None, None);
    };
    let geo = match redis_client::geo::read_one(conn, scooter_id).await {
        Ok(geo) => geo,
        Err(error) => {
            tracing::warn!(%error, %scooter_id, "geo hash lookup failed");
            None
        }
    };
    let Some(geo) = geo else {
        return (None, None);
    };
    (
        Some("scooter.updated"),
        Some(json!({
            "id": id,
            "lat": geo.lat,
            "lon": geo.lon,
            "status": status,
            "battery_pct": geo.battery_pct,
        })),
    )
}

/// Приватные события юзера: Kafka payload → (user_id, тип конверта, payload)
/// по websocket.md §4. `None` — маппинга нет.
fn map_user_event(topic: &str, payload: &Value) -> Option<(Uuid, &'static str, Value)> {
    let user_id = payload
        .get("user_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok());
    match topic {
        "booking.created.v1" => Some((
            user_id?,
            "reservation.created",
            json!({
                "id": payload.get("booking_id")?,
                "scooter_id": payload.get("scooter_id")?,
                "expires_at": payload.get("expires_at")?,
            }),
        )),
        "booking.expired.v1" => Some((
            user_id?,
            "reservation.expired",
            json!({
                "id": payload.get("booking_id")?,
                "reason": payload.get("reason")?,
            }),
        )),
        "rental.started.v1" => Some((
            user_id?,
            "ride.started",
            json!({
                "ride_id": payload.get("rental_id")?,
                "scooter_id": payload.get("scooter_id")?,
                "started_at": payload.get("started_at")?,
            }),
        )),
        "rental.finished.v1" => Some((
            user_id?,
            "ride.finished",
            json!({
                "ride_id": payload.get("rental_id")?,
                "total_min": payload.get("total_min")?,
                "amount_kopeks": payload.get("amount_kopeks")?,
            }),
        )),
        // ADR-0006: холд снят — фиксируем это в конверте для UI.
        "rental.unlock-failed.v1" => Some((
            user_id?,
            "ride.unlock_failed",
            json!({
                "ride_id": payload.get("rental_id")?,
                "reason": payload.get("reason")?,
                "hold_canceled": true,
            }),
        )),
        // payment.events.v1: event=payment.succeeded|payment.canceled
        // → статус из websocket.md §4 (hold|captured|canceled|refunded).
        "payment.events.v1" => {
            let status = match payload.get("event").and_then(Value::as_str)? {
                "payment.succeeded" => "captured",
                "payment.canceled" => "canceled",
                _ => return None,
            };
            Some((
                user_id?,
                "payment.status_changed",
                json!({
                    "payment_id": payload.get("payment_id")?,
                    "ride_id": payload.get("rental_id")?,
                    "status": status,
                }),
            ))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn topics_cover_asyncapi_public_set() {
        for topic in TOPICS {
            assert!(topic.ends_with(".v1"), "topic {topic} must be versioned");
        }
        assert!(TOPICS.contains(&"scooter.status.v1"));
        assert!(!TOPICS.contains(&"capture.retry.v1"));
    }

    #[test]
    fn booking_created_maps_to_reservation_created() {
        let user = Uuid::new_v4();
        let booking = Uuid::new_v4();
        let (owner, kind, payload) = map_user_event(
            "booking.created.v1",
            &json!({
                "booking_id": booking,
                "scooter_id": Uuid::new_v4(),
                "user_id": user,
                "expires_at": "2026-10-01T12:10:00Z",
            }),
        )
        .expect("mapping");
        assert_eq!(owner, user);
        assert_eq!(kind, "reservation.created");
        assert_eq!(payload["id"], json!(booking));
        assert!(payload["expires_at"].is_string());
    }

    #[test]
    fn booking_expired_keeps_reason() {
        let (_, kind, payload) = map_user_event(
            "booking.expired.v1",
            &json!({"booking_id": Uuid::new_v4(), "scooter_id": Uuid::new_v4(), "user_id": Uuid::new_v4(), "reason": "ttl"}),
        )
        .unwrap();
        assert_eq!(kind, "reservation.expired");
        assert_eq!(payload["reason"], "ttl");
    }

    #[test]
    fn rental_lifecycle_maps_to_ride_events() {
        let rental = Uuid::new_v4();
        let (_, kind, payload) = map_user_event(
            "rental.started.v1",
            &json!({"rental_id": rental, "scooter_id": Uuid::new_v4(), "user_id": Uuid::new_v4(), "started_at": "2026-10-01T12:00:00Z"}),
        )
        .unwrap();
        assert_eq!(kind, "ride.started");
        assert_eq!(payload["ride_id"], json!(rental));

        let (_, kind, payload) = map_user_event(
            "rental.finished.v1",
            &json!({"rental_id": rental, "scooter_id": Uuid::new_v4(), "user_id": Uuid::new_v4(), "total_min": 7, "amount_kopeks": 8500}),
        )
        .unwrap();
        assert_eq!(kind, "ride.finished");
        assert_eq!(payload["amount_kopeks"], 8500);
    }

    #[test]
    fn unlock_failed_marks_hold_canceled() {
        let (_, kind, payload) = map_user_event(
            "rental.unlock-failed.v1",
            &json!({"rental_id": Uuid::new_v4(), "scooter_id": Uuid::new_v4(), "user_id": Uuid::new_v4(), "reason": "lock_ack_timeout"}),
        )
        .unwrap();
        assert_eq!(kind, "ride.unlock_failed");
        assert_eq!(payload["reason"], "lock_ack_timeout");
        assert_eq!(payload["hold_canceled"], true);
    }

    #[test]
    fn payment_events_map_to_status_changed() {
        let payment = Uuid::new_v4();
        let rental = Uuid::new_v4();
        let base = json!({"payment_id": payment, "rental_id": rental, "user_id": Uuid::new_v4(), "amount": 8500});

        let mut succeeded = base.clone();
        succeeded["event"] = json!("payment.succeeded");
        let (_, kind, payload) = map_user_event("payment.events.v1", &succeeded).unwrap();
        assert_eq!(kind, "payment.status_changed");
        assert_eq!(payload["status"], "captured");
        assert_eq!(payload["payment_id"], json!(payment));
        assert_eq!(payload["ride_id"], json!(rental));

        let mut canceled = base;
        canceled["event"] = json!("payment.canceled");
        let (_, kind, payload) = map_user_event("payment.events.v1", &canceled).unwrap();
        assert_eq!(kind, "payment.status_changed");
        assert_eq!(payload["status"], "canceled");
    }

    #[test]
    fn unknown_payment_event_and_unknown_topic_are_skipped() {
        let mut refunded = json!({"payment_id": Uuid::new_v4(), "rental_id": Uuid::new_v4(), "user_id": Uuid::new_v4()});
        refunded["event"] = json!("payment.refunded");
        assert!(map_user_event("payment.events.v1", &refunded).is_none());
        assert!(map_user_event("scooter.telemetry.v1", &json!({})).is_none());
        assert!(
            map_user_event("rental.started.v1", &json!({"rental_id": Uuid::new_v4()})).is_none()
        );
    }

    #[tokio::test]
    #[ignore = "requires live Redis (make up)"]
    async fn scooter_status_becomes_map_envelope() {
        let redis_url = std::env::var("REDIS_URL").expect("REDIS_URL is not set");
        let client = redis::Client::open(redis_url.as_str()).unwrap();
        let mut conn = client.get_connection_manager().await.unwrap();

        let id = Uuid::new_v4();
        redis_client::geo::upsert(
            &mut conn,
            &redis_client::geo::GeoScooter {
                id,
                code: "BR-TEST".into(),
                lat: 43.238,
                lon: 76.889,
                status: "available".into(),
                battery_pct: 64,
            },
        )
        .await
        .unwrap();

        let (kind, payload) =
            map_scooter_status(&mut conn, &json!({"scooter_id": id, "status": "rented"})).await;
        let kind = kind.unwrap();
        let payload = payload.unwrap();
        assert_eq!(kind, "scooter.updated");
        assert_eq!(payload["status"], "rented");
        assert_eq!(payload["battery_pct"], 64);

        let (kind, payload) =
            map_scooter_status(&mut conn, &json!({"scooter_id": id, "status": "offline"})).await;
        assert_eq!(kind.unwrap(), "scooter.removed");
        assert_eq!(payload.unwrap()["reason"], "offline");

        // Неизвестный самокат (нет geo-хэша) — события карты нет.
        let (kind, payload) = map_scooter_status(
            &mut conn,
            &json!({"scooter_id": Uuid::new_v4(), "status": "available"}),
        )
        .await;
        assert!(kind.is_none() && payload.is_none());
    }
}

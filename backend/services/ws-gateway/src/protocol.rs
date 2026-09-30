//! Протокол ws-шлюза: конверт сообщений (websocket.md §2), client → server
//! команды, гео-фильтр подписок.

use chrono::SecondsFormat;
use serde_json::{json, Value};
use uuid::Uuid;

/// Радиус подписки по умолчанию, м (websocket.md §3).
pub const DEFAULT_RADIUS_M: f64 = 500.0;
/// Максимум радиуса подписки, м.
pub const MAX_RADIUS_M: f64 = 3000.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Subscription {
    pub lat: f64,
    pub lon: f64,
    pub radius_m: f64,
}

impl Subscription {
    /// Точка попадает в подписку (граница включена).
    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        haversine_m(self.lat, self.lon, lat, lon) <= self.radius_m
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClientMsg {
    Ping,
    Subscribe { lat: f64, lon: f64, radius_m: f64 },
    Unsubscribe,
    TrackRide { ride_id: Uuid },
}

/// Разбор client → server сообщения. Err — человекочитаемая причина
/// для конверта `error` с `code=bad_message`.
pub fn parse_client_message(raw: &str) -> Result<ClientMsg, String> {
    let value: Value = serde_json::from_str(raw).map_err(|_| "invalid json".to_owned())?;
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing type".to_owned())?;
    let payload = value.get("payload").cloned().unwrap_or(Value::Null);

    match kind {
        "ping" => Ok(ClientMsg::Ping),
        "unsubscribe.scooters" => Ok(ClientMsg::Unsubscribe),
        "subscribe.scooters" => {
            let lat = payload
                .get("lat")
                .and_then(Value::as_f64)
                .ok_or_else(|| "subscribe.scooters: missing lat".to_owned())?;
            let lon = payload
                .get("lon")
                .and_then(Value::as_f64)
                .ok_or_else(|| "subscribe.scooters: missing lon".to_owned())?;
            let radius_m = payload
                .get("radius_m")
                .and_then(Value::as_f64)
                .unwrap_or(DEFAULT_RADIUS_M);
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                return Err("subscribe.scooters: lat/lon out of range".to_owned());
            }
            if !(0.0..MAX_RADIUS_M).contains(&radius_m) {
                return Err(format!(
                    "subscribe.scooters: radius_m must be within (0, {MAX_RADIUS_M}]"
                ));
            }
            Ok(ClientMsg::Subscribe { lat, lon, radius_m })
        }
        "track.ride" => {
            let ride_id = payload
                .get("ride_id")
                .and_then(Value::as_str)
                .ok_or_else(|| "track.ride: missing ride_id".to_owned())?;
            let ride_id = Uuid::parse_str(ride_id)
                .map_err(|_| "track.ride: ride_id must be uuid".to_owned())?;
            Ok(ClientMsg::TrackRide { ride_id })
        }
        other => Err(format!("unknown type: {other}")),
    }
}

/// Конверт серверного сообщения: `{"type","id","ts","payload"}` (websocket.md §2).
pub fn envelope(kind: &str, payload: Value) -> String {
    json!({
        "type": kind,
        "id": Uuid::new_v4(),
        "ts": chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        "payload": payload,
    })
    .to_string()
}

/// Большой круг, метры (WGS84-сфероид не нужен — для фильтра карты хватает сферы).
pub fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const EARTH_RADIUS_M: f64 = 6_371_000.0;
    let (lat1, lat2) = (lat1.to_radians(), lat2.to_radians());
    let dlat = lat2 - lat1;
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * a.sqrt().asin()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ping() {
        assert_eq!(
            parse_client_message(r#"{"type":"ping"}"#),
            Ok(ClientMsg::Ping)
        );
    }

    #[test]
    fn parses_subscribe_with_default_radius() {
        let msg = parse_client_message(
            r#"{"type":"subscribe.scooters","payload":{"lat":43.238,"lon":76.889}}"#,
        )
        .unwrap();
        assert_eq!(
            msg,
            ClientMsg::Subscribe {
                lat: 43.238,
                lon: 76.889,
                radius_m: DEFAULT_RADIUS_M
            }
        );
    }

    #[test]
    fn rejects_subscribe_out_of_range() {
        let raw = r#"{"type":"subscribe.scooters","payload":{"lat":91,"lon":0}}"#;
        assert!(parse_client_message(raw).is_err());
        let raw = r#"{"type":"subscribe.scooters","payload":{"lat":0,"lon":0,"radius_m":3001}}"#;
        assert!(parse_client_message(raw).is_err());
    }

    #[test]
    fn rejects_unknown_and_malformed() {
        assert!(parse_client_message("not json").is_err());
        assert!(parse_client_message(r#"{"type":"meh"}"#).is_err());
        assert!(parse_client_message(r#"{"payload":{}}"#).is_err());
    }

    #[test]
    fn parses_track_ride() {
        let ride = Uuid::new_v4();
        let raw = format!(r#"{{"type":"track.ride","payload":{{"ride_id":"{ride}"}}}}"#);
        assert_eq!(
            parse_client_message(&raw),
            Ok(ClientMsg::TrackRide { ride_id: ride })
        );
    }

    #[test]
    fn subscription_contains_by_radius() {
        let sub = Subscription {
            lat: 43.238,
            lon: 76.889,
            radius_m: 500.0,
        };
        assert!(sub.contains(43.2385, 76.8895)); // ~70 м
        assert!(!sub.contains(43.25, 76.889)); // ~1.3 км
    }

    #[test]
    fn haversine_known_distances() {
        // 0.01° широты ≈ 1113 м.
        let d = haversine_m(43.0, 76.889, 43.01, 76.889);
        assert!((d - 1113.0).abs() < 15.0, "got {d}");
        // Нулевое расстояние.
        assert!(haversine_m(43.0, 76.0, 43.0, 76.0) < f64::EPSILON);
    }

    #[test]
    fn envelope_has_wrapper_fields() {
        let env: Value = serde_json::from_str(&envelope("pong", json!({}))).unwrap();
        assert_eq!(env["type"], "pong");
        assert!(env["id"].as_str().is_some());
        assert!(env["ts"].as_str().is_some());
        assert_eq!(env["payload"], json!({}));
    }
}

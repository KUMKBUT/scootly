//! GEO-кэш самокатов (ADR-0011): `GEOADD scooters:geo` + метаданные в
//! `scooter:{id}` (TTL 60 c — свежесть выдачи). Пишут telemetry-service и seeder,
//! читает geo-service. Все записи — пайплайном.

use redis::aio::ConnectionManager;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// GEO-индекс: member = scooter uuid, score = geohash.
pub const GEO_KEY: &str = "scooters:geo";
/// TTL хэша метаданных: после 60 c без обновления самокат исчезает из выдачи (ADR-0011).
pub const SCOOTER_TTL_SECS: u64 = 60;
/// Канал Redis pub/sub для WS-фан-аута карты (см. docs/api/websocket.md §7).
pub const WS_SCOOTERS_CHANNEL: &str = "ws:scooters";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoScooter {
    pub id: Uuid,
    pub code: String,
    pub lat: f64,
    pub lon: f64,
    pub status: String,
    pub battery_pct: i32,
}

/// Попадание GEOSEARCH без метаданных (метаданные — отдельным пайплайном HGETALL).
#[derive(Debug, Clone, Serialize)]
pub struct GeoHit {
    pub id: Uuid,
    pub lat: f64,
    pub lon: f64,
    /// Метры (единство с единицей запроса BYRADIUS ... m).
    pub distance_m: f64,
}

pub fn scooter_key(id: Uuid) -> String {
    format!("scooter:{id}")
}

/// GEOADD + HSET + EXPIRE одним пайплайном.
pub async fn upsert(conn: &mut ConnectionManager, scooter: &GeoScooter) -> anyhow::Result<()> {
    let mut pipe = redis::pipe();
    pipe.cmd("GEOADD")
        .arg(GEO_KEY)
        .arg(scooter.lon)
        .arg(scooter.lat)
        .arg(scooter.id.to_string());
    pipe.cmd("HSET")
        .arg(scooter_key(scooter.id))
        .arg("code")
        .arg(&scooter.code)
        .arg("status")
        .arg(&scooter.status)
        .arg("battery_pct")
        .arg(scooter.battery_pct)
        .arg("lat")
        .arg(scooter.lat)
        .arg("lon")
        .arg(scooter.lon);
    pipe.cmd("EXPIRE")
        .arg(scooter_key(scooter.id))
        .arg(SCOOTER_TTL_SECS as i64);
    pipe.query_async::<_, ()>(conn).await?;
    Ok(())
}

/// Убирает самокат из выдачи (status → offline и т.п.).
pub async fn remove(conn: &mut ConnectionManager, id: Uuid) -> anyhow::Result<()> {
    let mut pipe = redis::pipe();
    pipe.cmd("ZREM").arg(GEO_KEY).arg(id.to_string());
    pipe.cmd("DEL").arg(scooter_key(id));
    pipe.query_async::<_, ()>(conn).await?;
    Ok(())
}

/// GEOSEARCH: попадания в радиусе с координатами и дистанцией, ближайшие первыми.
pub async fn search(
    conn: &mut ConnectionManager,
    lat: f64,
    lon: f64,
    radius_m: u32,
    limit: usize,
) -> anyhow::Result<Vec<GeoHit>> {
    // Ответ GEOSEARCH ... WITHDIST WITHCOORD — список строк [member, dist,
    // [lon, lat]]. Типы-кортежи тут не годятся: в redis-rs 0.25 `Vec<(A, B, C)>`
    // трактует bulk как ПЛОСКИЙ список (chunks_exact по размеру кортежа) и
    // валится на вложенных строках («wrong dimension»), поэтому строку
    // разбираем вручную.
    let rows: Vec<Vec<redis::Value>> = redis::cmd("GEOSEARCH")
        .arg(GEO_KEY)
        .arg("FROMLONLAT")
        .arg(lon)
        .arg(lat)
        .arg("BYRADIUS")
        .arg(radius_m)
        .arg("m")
        .arg("ASC")
        .arg("COUNT")
        .arg(limit)
        .arg("WITHDIST")
        .arg("WITHCOORD")
        .query_async(conn)
        .await?;

    Ok(rows
        .into_iter()
        .filter_map(|row| {
            if row.len() != 3 {
                return None;
            }
            let member: String = redis::from_redis_value(&row[0]).ok()?;
            let id = Uuid::parse_str(&member).ok()?;
            let distance_m: f64 = redis::from_redis_value(&row[1]).ok()?;
            let coord: Vec<f64> = redis::from_redis_value(&row[2]).ok()?;
            let (lon, lat) = (*coord.first()?, *coord.get(1)?);
            Some(GeoHit {
                id,
                lat,
                lon,
                distance_m,
            })
        })
        .collect())
}

/// Догружает метаданные пайплайном HGETALL; самокаты с истёкшим TTL (нет хэша)
/// пропускаются — их добирает fallback к scooter-service (ADR-0011).
pub async fn hydrate(
    conn: &mut ConnectionManager,
    hits: &[GeoHit],
) -> anyhow::Result<Vec<(GeoHit, Option<GeoScooter>)>> {
    if hits.is_empty() {
        return Ok(Vec::new());
    }

    let mut pipe = redis::pipe();
    for hit in hits {
        pipe.cmd("HGETALL").arg(scooter_key(hit.id));
    }
    let hashes: Vec<std::collections::HashMap<String, String>> = pipe.query_async(conn).await?;

    Ok(hits
        .iter()
        .cloned()
        .zip(hashes)
        .map(|(hit, fields)| {
            let scooter =
                parse_hash(&fields).map(|(code, status, battery_pct, lat, lon)| GeoScooter {
                    id: hit.id,
                    code,
                    lat,
                    lon,
                    status,
                    battery_pct,
                });
            (hit, scooter)
        })
        .collect())
}

type HashFields = (String, String, i32, f64, f64);

fn parse_hash(fields: &std::collections::HashMap<String, String>) -> Option<HashFields> {
    let code = fields.get("code")?.clone();
    let status = fields.get("status")?.clone();
    let battery_pct = fields.get("battery_pct")?.parse().ok()?;
    // Координаты берём из хэша: они записаны тем же обновлением, что и статус.
    let lat: f64 = fields.get("lat")?.parse().ok()?;
    let lon: f64 = fields.get("lon")?.parse().ok()?;
    Some((code, status, battery_pct, lat, lon))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scooter_key_format() {
        let id = Uuid::new_v4();
        assert_eq!(scooter_key(id), format!("scooter:{id}"));
    }

    #[test]
    fn parse_hash_requires_all_fields() {
        let full = [
            ("code".to_owned(), "SC-0001".to_owned()),
            ("status".to_owned(), "available".to_owned()),
            ("battery_pct".to_owned(), "87".to_owned()),
            ("lat".to_owned(), "43.238".to_owned()),
            ("lon".to_owned(), "76.889".to_owned()),
        ]
        .into_iter()
        .collect();
        assert!(parse_hash(&full).is_some());

        let missing = [
            ("code".to_owned(), "SC-0001".to_owned()),
            ("status".to_owned(), "available".to_owned()),
        ]
        .into_iter()
        .collect();
        assert!(parse_hash(&missing).is_none());
    }
}

//! Репозиторий `scooters`. Никаких прямых SQL-запросов вне crates/db.

use common::{AppError, AppResult};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Scooter {
    pub id: Uuid,
    pub code: String,
    pub lat: f64,
    pub lon: f64,
    pub status: String,
    pub battery_pct: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Строка поиска вокруг точки (ADR-0011 fallback: last_position из PG).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ScooterNearby {
    pub id: Uuid,
    pub code: String,
    pub lat: f64,
    pub lon: f64,
    pub status: String,
    pub battery_pct: i32,
    /// Расстояние от центра запроса, метры.
    pub distance_m: f64,
}

#[derive(Debug, Clone)]
pub struct NewScooter {
    pub code: String,
    pub lat: f64,
    pub lon: f64,
    pub status: String,
    pub battery_pct: i32,
}

/// Идемпотентный сидер: по `code` обновляем позицию/статус/батарею.
pub async fn upsert_by_code(pool: &PgPool, scooter: &NewScooter) -> AppResult<Scooter> {
    sqlx::query_as::<_, Scooter>(
        r#"
        INSERT INTO scooters (code, lat, lon, status, battery_pct)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (code) DO UPDATE
            SET lat = EXCLUDED.lat,
                lon = EXCLUDED.lon,
                status = EXCLUDED.status,
                battery_pct = EXCLUDED.battery_pct
        RETURNING id, code, lat, lon, status, battery_pct, created_at
        "#,
    )
    .bind(&scooter.code)
    .bind(scooter.lat)
    .bind(scooter.lon)
    .bind(&scooter.status)
    .bind(scooter.battery_pct)
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))
}

/// Радиус в градусах широты/долготы для предварительного box-фильтра.
pub fn bounding_box(lat: f64, lon: f64, radius_m: u32) -> (f64, f64, f64, f64) {
    const METERS_PER_DEG_LAT: f64 = 111_320.0;
    let lat_delta = f64::from(radius_m) / METERS_PER_DEG_LAT;
    let lon_delta = f64::from(radius_m) / (METERS_PER_DEG_LAT * lat.to_radians().cos().max(0.01));
    (
        lat - lat_delta,
        lat + lat_delta,
        lon - lon_delta,
        lon + lon_delta,
    )
}

/// Самокаты вокруг точки, ближайшие первыми. Offline не выдаётся.
/// `ids` пустой → искать все в радиусе; иначе только перечисленные
/// (кейс «в GEO-кэше нет записи по самокату», ADR-0011).
pub async fn find_nearby(
    pool: &PgPool,
    lat: f64,
    lon: f64,
    radius_m: u32,
    ids: &[Uuid],
    limit: i64,
) -> AppResult<Vec<ScooterNearby>> {
    let (lat_min, lat_max, lon_min, lon_max) = bounding_box(lat, lon, radius_m);
    sqlx::query_as::<_, ScooterNearby>(
        r#"
        SELECT id, code, lat, lon, status, battery_pct, distance_m
        FROM (
            SELECT id, code, lat, lon, status, battery_pct,
                   6371000.0 * 2.0 * asin(sqrt(
                       power(sin(radians(lat - $1) / 2.0), 2)
                       + cos(radians($1)) * cos(radians(lat))
                         * power(sin(radians(lon - $2) / 2.0), 2)
                   )) AS distance_m
            FROM scooters
            WHERE status <> 'offline'
              AND ($5::uuid[] IS NULL OR id = ANY($5))
              AND lat BETWEEN $6 AND $7
              AND lon BETWEEN $8 AND $9
        ) nearby
        WHERE distance_m <= $3
        ORDER BY distance_m
        LIMIT $4
        "#,
    )
    .bind(lat)
    .bind(lon)
    .bind(f64::from(radius_m))
    .bind(limit)
    .bind(ids_option(ids))
    .bind(lat_min)
    .bind(lat_max)
    .bind(lon_min)
    .bind(lon_max)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))
}

fn ids_option(ids: &[Uuid]) -> Option<Vec<Uuid>> {
    if ids.is_empty() {
        None
    } else {
        Some(ids.to_vec())
    }
}

/// Чистка за собой в тестах.
pub async fn delete_by_codes(pool: &PgPool, codes: &[String]) -> AppResult<u64> {
    let result = sqlx::query("DELETE FROM scooters WHERE code = ANY($1)")
        .bind(codes)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(result.rows_affected())
}

pub async fn find_by_code(pool: &PgPool, code: &str) -> AppResult<Scooter> {
    sqlx::query_as::<_, Scooter>(
        "SELECT id, code, lat, lon, status, battery_pct, created_at FROM scooters WHERE code = $1",
    )
    .bind(code)
    .fetch_one(pool)
    .await
    .map_err(|e| match e {
        sqlx::Error::RowNotFound => AppError::NotFound(format!("scooter {code}")),
        other => AppError::Internal(other.into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounding_box_covers_radius() {
        // Almaty, 500 м: градусы широты ~ 0.0045.
        let (lat_min, lat_max, lon_min, lon_max) = bounding_box(43.238, 76.889, 500);
        assert!((lat_max - lat_min) > 0.008);
        assert!((lon_max - lon_min) > 0.008);
        assert!(lat_min < 43.238 && 43.238 < lat_max);
        assert!(lon_min < 76.889 && 76.889 < lon_max);
    }

    #[test]
    fn ids_option_none_on_empty() {
        assert!(ids_option(&[]).is_none());
        let id = Uuid::new_v4();
        assert_eq!(ids_option(&[id]), Some(vec![id]));
    }
}

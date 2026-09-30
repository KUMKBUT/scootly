//! Выдача самокатов рядом: Redis GEO — горячий путь, scooter-service gRPC —
//! fallback (ADR-0011). Деградация предсказуема: cache unavailable или
//! fallback error → отдаём то, что есть, без 5xx.

use std::collections::HashMap;
use std::time::Duration;

use proto::scootly::scooter::v1::LastPositionsRequest;
use tonic::Request;
use uuid::Uuid;

use crate::dto::{ScooterDto, SEARCH_LIMIT};
use crate::AppState;

/// Потолок ожидания fallback'а: карта не должна зависнуть на недоступном
/// scooter-service (resilience, ADR-0011/0014).
const FALLBACK_TIMEOUT: Duration = Duration::from_millis(1500);

/// (расстояние в метрах, DTO) — для сортировки при слиянии cache + fallback.
type Entry = (f64, ScooterDto);

pub async fn nearby(
    state: &AppState,
    lat: f64,
    lon: f64,
    radius_m: u32,
) -> common::AppResult<Vec<ScooterDto>> {
    let mut merged: HashMap<Uuid, Entry> = HashMap::new();

    match cache_hits(state, lat, lon, radius_m).await {
        CacheResult::Hits(hits) if hits.is_empty() => {
            // Холодный кэш: весь запрос уходит в fallback по радиусу.
            fallback(state, lat, lon, radius_m, &[], &mut merged).await;
        }
        CacheResult::Hits(hits) => {
            let (hydrated, missing) = hydrate(state, hits).await;
            for (hit, scooter) in hydrated {
                if let Some(scooter) = scooter {
                    merged.insert(scooter.id, (hit.distance_m, scooter.into()));
                }
            }
            // TTL у хэша истёк → этих id в кэше нет — добираем из PG (ADR-0011).
            if !missing.is_empty() {
                let missing: Vec<Uuid> = missing.into_iter().collect();
                fallback(state, lat, lon, radius_m, &missing, &mut merged).await;
            }
        }
        CacheResult::Unavailable => {
            fallback(state, lat, lon, radius_m, &[], &mut merged).await;
        }
    }

    let mut items: Vec<Entry> = merged.into_values().collect();
    items.sort_by(|a, b| a.0.total_cmp(&b.0));
    items.truncate(SEARCH_LIMIT);
    Ok(items.into_iter().map(|(_, scooter)| scooter).collect())
}

enum CacheResult {
    Hits(Vec<redis_client::geo::GeoHit>),
    Unavailable,
}

async fn cache_hits(state: &AppState, lat: f64, lon: f64, radius_m: u32) -> CacheResult {
    let result = match state.redis.get().await {
        Ok(mut conn) => {
            redis_client::geo::search(&mut conn, lat, lon, radius_m, SEARCH_LIMIT).await
        }
        Err(error) => Err(error),
    };
    match result {
        Ok(hits) => CacheResult::Hits(hits),
        Err(error) => {
            tracing::warn!(%error, "geo cache unavailable, falling back to scooter-service");
            state.redis.invalidate().await;
            CacheResult::Unavailable
        }
    }
}

/// Возвращает (попадания с метаданными, id без метаданных — TTL истёк).
async fn hydrate(
    state: &AppState,
    hits: Vec<redis_client::geo::GeoHit>,
) -> (
    Vec<(
        redis_client::geo::GeoHit,
        Option<redis_client::geo::GeoScooter>,
    )>,
    std::collections::HashSet<Uuid>,
) {
    let hydrated = match state.redis.get().await {
        Ok(mut conn) => redis_client::geo::hydrate(&mut conn, &hits).await,
        Err(error) => {
            tracing::warn!(%error, "geo cache unavailable (hydrate)");
            Err(error)
        }
    };
    match hydrated {
        Ok(pairs) => {
            let missing = pairs
                .iter()
                .filter(|(_, scooter)| scooter.is_none())
                .map(|(hit, _)| hit.id)
                .collect();
            (pairs, missing)
        }
        Err(error) => {
            tracing::warn!(%error, "geo hydrate failed, treating all hits as missing");
            state.redis.invalidate().await;
            let pairs = hits.into_iter().map(|hit| (hit, None)).collect::<Vec<_>>();
            let missing = pairs.iter().map(|(hit, _)| hit.id).collect();
            (pairs, missing)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn fallback(
    state: &AppState,
    lat: f64,
    lon: f64,
    radius_m: u32,
    ids: &[Uuid],
    merged: &mut HashMap<Uuid, Entry>,
) {
    let request = LastPositionsRequest {
        lat,
        lon,
        radius_m,
        scooter_ids: ids.iter().map(ToString::to_string).collect(),
        limit: SEARCH_LIMIT as u32,
    };
    let mut client = state.scooters.clone();
    let rpc = client.last_positions(Request::new(request));
    match tokio::time::timeout(FALLBACK_TIMEOUT, rpc).await {
        Ok(Ok(response)) => {
            for position in response.into_inner().positions {
                let dto = ScooterDto::from(&position);
                // Кэш главнее fallback: он свежее по позиции.
                if let Ok(id) = Uuid::parse_str(&position.id) {
                    merged.entry(id).or_insert((position.distance_m, dto));
                }
            }
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, "scooter-service fallback failed");
        }
        Err(_) => {
            tracing::warn!(
                timeout_ms = FALLBACK_TIMEOUT.as_millis() as u64,
                "scooter-service fallback timed out"
            );
        }
    }
}

//! DTO `/api/v1/scooters/nearby` — схема `Scooter` из docs/api/openapi.yaml.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Радиус поиска по умолчанию (openapi: default 500, maximum 3000).
pub const DEFAULT_RADIUS_M: u32 = 500;
pub const MAX_RADIUS_M: u32 = 3000;
/// Жёсткий предел выдачи одного запроса.
pub const SEARCH_LIMIT: usize = 200;

#[derive(Debug, Deserialize)]
pub struct NearbyQuery {
    pub lat: f64,
    pub lon: f64,
    pub radius_m: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
pub struct ValidatedNearby {
    pub lat: f64,
    pub lon: f64,
    pub radius_m: u32,
}

impl NearbyQuery {
    /// Валидация по границам openapi: lat -90..90, lon -180..180, radius 1..3000.
    pub fn validate(self) -> Result<ValidatedNearby, common::AppError> {
        if !(-90.0..=90.0).contains(&self.lat) {
            return Err(common::AppError::Validation(
                "lat must be within [-90, 90]".into(),
            ));
        }
        if !(-180.0..=180.0).contains(&self.lon) {
            return Err(common::AppError::Validation(
                "lon must be within [-180, 180]".into(),
            ));
        }
        let radius_m = self.radius_m.unwrap_or(f64::from(DEFAULT_RADIUS_M));
        if !(1.0..=f64::from(MAX_RADIUS_M)).contains(&radius_m) {
            return Err(common::AppError::Validation(format!(
                "radius_m must be within [1, {MAX_RADIUS_M}]"
            )));
        }
        Ok(ValidatedNearby {
            lat: self.lat,
            lon: self.lon,
            radius_m: radius_m as u32,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScooterDto {
    pub id: Uuid,
    pub code: String,
    pub lat: f64,
    pub lon: f64,
    pub status: String,
    pub battery_pct: i32,
    /// Заполняется rental-service (бронь текущего юзера); geo не знает про брони.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub my_reservation_id: Option<Uuid>,
}

impl From<redis_client::geo::GeoScooter> for ScooterDto {
    fn from(s: redis_client::geo::GeoScooter) -> Self {
        Self {
            id: s.id,
            code: s.code,
            lat: s.lat,
            lon: s.lon,
            status: s.status,
            battery_pct: s.battery_pct,
            my_reservation_id: None,
        }
    }
}

impl From<&proto::scootly::scooter::v1::ScooterPosition> for ScooterDto {
    fn from(p: &proto::scootly::scooter::v1::ScooterPosition) -> Self {
        Self {
            id: Uuid::parse_str(&p.id).unwrap_or_else(|_| Uuid::nil()),
            code: p.code.clone(),
            lat: p.lat,
            lon: p.lon,
            status: p.status.clone(),
            battery_pct: p.battery_pct.clamp(0, 100) as i32,
            my_reservation_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(lat: f64, lon: f64, radius_m: Option<f64>) -> NearbyQuery {
        NearbyQuery { lat, lon, radius_m }
    }

    #[test]
    fn default_radius() {
        let v = query(43.238, 76.889, None).validate().unwrap();
        assert_eq!(v.radius_m, DEFAULT_RADIUS_M);
    }

    #[test]
    fn rejects_out_of_range() {
        assert!(query(90.5, 0.0, None).validate().is_err());
        assert!(query(-90.5, 0.0, None).validate().is_err());
        assert!(query(0.0, 180.5, None).validate().is_err());
        assert!(query(0.0, 0.0, Some(0.0)).validate().is_err());
        assert!(query(0.0, 0.0, Some(f64::from(MAX_RADIUS_M) + 1.0))
            .validate()
            .is_err());
    }

    #[test]
    fn accepts_boundaries() {
        assert!(query(90.0, -180.0, Some(1.0)).validate().is_ok());
        assert!(query(-90.0, 180.0, Some(f64::from(MAX_RADIUS_M)))
            .validate()
            .is_ok());
    }
}

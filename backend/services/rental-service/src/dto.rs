//! DTO `/api/v1/reservations` — схема `Reservation` из docs/api/openapi.yaml.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct CreateReservation {
    pub scooter_id: Uuid,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReservationDto {
    pub id: Uuid,
    pub scooter_id: Uuid,
    /// `active | expired | converted | canceled` (openapi); `canceled` — наша
    /// отметка ручной отмены, в БД она отдельным статусом (`bookings.status`).
    pub status: String,
    pub expires_at: DateTime<Utc>,
}

impl From<db::bookings::Booking> for ReservationDto {
    fn from(b: db::bookings::Booking) -> Self {
        Self {
            id: b.id,
            scooter_id: b.scooter_id,
            status: b.status,
            expires_at: b.expires_at,
        }
    }
}

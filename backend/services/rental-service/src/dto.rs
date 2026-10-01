//! DTO `/api/v1/rides` и `/api/v1/reservations` — схемы `Ride`/`Reservation`
//! из docs/api/openapi.yaml.

use chrono::{DateTime, Utc};
use common::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::services::tariff::Tariff;

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

/// openapi startRide: `scooter_id` обязателен, `reservation_id` — если старт из брони.
#[derive(Debug, Deserialize)]
pub struct StartRide {
    pub scooter_id: Uuid,
    pub reservation_id: Option<Uuid>,
}

/// openapi finishRide: координаты — для истории.
#[derive(Debug, Deserialize)]
pub struct FinishRide {
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

impl FinishRide {
    /// Координаты принимаем только парой: одна без другой портит историю.
    pub fn validated(self) -> AppResult<(Option<f64>, Option<f64>)> {
        match (self.lat, self.lon) {
            (Some(lat), Some(lon)) => {
                if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                    return Err(AppError::Validation("lat/lon out of range".into()));
                }
                Ok((Some(lat), Some(lon)))
            }
            (None, None) => Ok((None, None)),
            _ => Err(AppError::Validation(
                "lat and lon must come together".into(),
            )),
        }
    }
}

/// openapi listRides: курсорная пагинация.
#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    pub limit: Option<i64>,
    pub before: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct RidesPageDto {
    pub items: Vec<RideDto>,
    pub next_before: Option<DateTime<Utc>>,
}

/// openapi `Payment` — блок оплаты в чеке поездки (MVP #5).
#[derive(Debug, Clone, Serialize)]
pub struct PaymentDto {
    pub id: Uuid,
    pub ride_id: Option<Uuid>,
    /// `hold | captured | canceled | refunded`.
    pub status: String,
    pub amount_kopeks: i64,
}

impl From<crate::services::payments::PaymentRecord> for PaymentDto {
    fn from(p: crate::services::payments::PaymentRecord) -> Self {
        Self {
            id: p.id,
            ride_id: Some(p.rental_id),
            status: p.status,
            amount_kopeks: p.amount_kopeks,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RideDto {
    pub id: Uuid,
    pub scooter_id: Uuid,
    pub reservation_id: Option<Uuid>,
    /// `active | finished | failed` (openapi).
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub total_min: Option<i32>,
    /// Для active — текущая стоимость на момент ответа (тик в UI).
    pub amount_kopeks: Option<i32>,
    /// Чек: холд на активной поездке, capture на завершённой (MVP #5).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment: Option<PaymentDto>,
}

impl RideDto {
    /// Активной поездке стоимость считает тариф на `now` (сервер — истина),
    /// завершённой отдаёт зафиксированные на финише значения.
    pub fn from_rental(rental: db::rentals::Rental, tariff: Tariff) -> Self {
        let (total_min, amount_kopeks) = if rental.status == "active" {
            let cost = tariff.cost(rental.started_at, Utc::now());
            (Some(cost.total_min), Some(cost.amount_kopeks))
        } else {
            (rental.total_min, rental.amount_kopeks)
        };
        Self {
            id: rental.id,
            scooter_id: rental.scooter_id,
            reservation_id: rental.reservation_id,
            status: rental.status,
            started_at: rental.started_at,
            finished_at: rental.finished_at,
            total_min,
            amount_kopeks,
            payment: None,
        }
    }

    /// Навешивает платёж (чек) из payment-service.
    pub fn with_payment(
        mut self,
        payment: Option<crate::services::payments::PaymentRecord>,
    ) -> Self {
        self.payment = payment.map(PaymentDto::from);
        self
    }
}

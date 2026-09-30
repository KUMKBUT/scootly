//! Брони (MVP #3): бесплатная бронь на 10 минут, TTL-автоснятие, отмена вручную.
//!
//! Гонка решается в БД (`UPDATE..WHERE..RETURNING` + частичные UNIQUE, ADR-0003/0015);
//! Redis-триггер и публикация — best-effort после коммита: PostgreSQL — source of
//! truth, расхождение закрывает джоб сверки ([`sweep_expired`], docs/mvp.md §5.1).

use std::time::Duration;

use common::AppResult;
use uuid::Uuid;

use crate::dto::ReservationDto;
use crate::{AppState, RESERVATION_TTL_SECS};

fn ttl() -> chrono::Duration {
    chrono::Duration::seconds(RESERVATION_TTL_SECS)
}

/// `POST /api/v1/reservations`: 201 + бронь, 404/409 — по контракту openapi.
#[tracing::instrument(skip_all, fields(user_id = %user_id, scooter_id = %scooter_id))]
pub async fn create(
    state: &AppState,
    user_id: Uuid,
    scooter_id: Uuid,
) -> AppResult<ReservationDto> {
    let booking = db::bookings::create(&state.pool, user_id, scooter_id, ttl()).await?;

    // TTL-триггер — best-effort: недоступный Redis не мешает брони (§5.1).
    match state.redis.get().await {
        Ok(mut conn) => {
            if let Err(error) = redis_client::bookings::arm(
                &mut conn,
                booking.id,
                Duration::from_secs(RESERVATION_TTL_SECS as u64),
            )
            .await
            {
                tracing::warn!(%error, booking_id = %booking.id, "redis ttl trigger not armed");
            }
        }
        Err(error) => {
            tracing::warn!(%error, booking_id = %booking.id, "redis unavailable, ttl trigger skipped");
        }
    }

    tracing::info!(booking_id = %booking.id, expires_at = %booking.expires_at, "reservation created");
    Ok(booking.into())
}

/// `DELETE /api/v1/reservations/{id}`: 204 идемпотентно, 404 — чужая/несуществующая.
#[tracing::instrument(skip_all, fields(user_id = %user_id, reservation_id = %reservation_id))]
pub async fn cancel(state: &AppState, user_id: Uuid, reservation_id: Uuid) -> AppResult<()> {
    match db::bookings::cancel(&state.pool, user_id, reservation_id).await? {
        db::bookings::CancelOutcome::Canceled { .. } => {
            disarm_trigger(state, reservation_id).await;
            tracing::info!(booking_id = %reservation_id, "reservation canceled");
        }
        db::bookings::CancelOutcome::AlreadyTerminal => {
            disarm_trigger(state, reservation_id).await;
            tracing::debug!(booking_id = %reservation_id, "reservation already terminal");
        }
    }
    Ok(())
}

async fn disarm_trigger(state: &AppState, booking_id: Uuid) {
    if let Ok(mut conn) = state.redis.get().await {
        if let Err(error) = redis_client::bookings::disarm(&mut conn, booking_id).await {
            tracing::warn!(%error, %booking_id, "redis ttl trigger not disarmed");
        }
    }
}

/// Один проход джоба сверки: истёкшие брони → `expired`, самокаты → `available`.
/// Возвращает число снятых броней.
#[tracing::instrument(skip_all)]
pub async fn sweep_expired(state: &AppState) -> AppResult<usize> {
    let swept = db::bookings::sweep_expired(&state.pool).await?;
    for expired in &swept {
        disarm_trigger(state, expired.booking.id).await;
        tracing::info!(
            booking_id = %expired.booking.id,
            scooter_id = %expired.booking.scooter_id,
            scooter_released = expired.scooter_released,
            "reservation expired by ttl"
        );
    }
    Ok(swept.len())
}

/// Фоновый цикл джоба сверки (ADR-0003): PG — истина, Redis-триггер может врать.
pub async fn run_sweeper(state: AppState, interval: Duration) {
    loop {
        tokio::time::sleep(interval).await;
        if let Err(error) = sweep_expired(&state).await {
            tracing::warn!(%error, "reservation sweep failed");
        }
    }
}

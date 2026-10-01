//! Поездки (MVP #4): старт из брони или напрямую (unlock), тик стоимости,
//! финиш (лок + расчёт). Схема статусов — openapi `Ride`: active | finished | failed.
//!
//! Оплата (MVP #5, ADR-0003/0014): холд YooKassa на старте (порядок контракта
//! холд → unlock → 201), capture с ключом `ride:{rental_id}` на финише.

use common::{AppError, AppResult};
use uuid::Uuid;

use crate::dto::StartRide;
use crate::services::payments::PaymentGateway;
use crate::{AppState, MAX_HISTORY_LIMIT};

/// `POST /api/v1/rides`: 201 + поездка, 402/404/409 — по контракту openapi.
#[tracing::instrument(skip_all, fields(user_id = %user_id))]
pub async fn start(
    state: &AppState,
    user_id: Uuid,
    req: StartRide,
) -> AppResult<db::rentals::Rental> {
    let rental_id = Uuid::new_v4();
    let hold_id = format!("hold:{rental_id}");
    let rental = match req.reservation_id {
        Some(reservation_id) => {
            db::rentals::create_from_reservation(
                &state.pool,
                user_id,
                reservation_id,
                rental_id,
                &hold_id,
            )
            .await?
        }
        None => {
            db::rentals::create_direct(&state.pool, user_id, req.scooter_id, rental_id, &hold_id)
                .await?
        }
    };

    // Порядок контракта: холд → unlock → 201. Холд на максимум оценки тарифа;
    // отказ (402 no_payment_method / hold_failed) — компенсация старта: поездка
    // failed, самокат снова available (ADR-0003; полная версия — MVP #6).
    let hold_amount = state.tariff.hold_amount();
    if let Err(error) = state.payments.hold(user_id, rental.id, hold_amount).await {
        tracing::warn!(%error, rental_id = %rental.id, "hold rejected, compensating start");
        db::rentals::fail_hold(&state.pool, rental.id).await?;
        return Err(error);
    }

    // Отказ unlock (502 unlock_timeout + компенсация: холд снят, юзер
    // уведомлён, самокат offline) — MVP #6 (ADR-0006).
    crate::services::locks::LockGateway::unlock(&state.locks, rental.scooter_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, rental_id = %rental.id, "unlock failed after start");
            AppError::Upstream {
                code: "unlock_timeout",
                message: "scooter did not confirm unlock".into(),
            }
        })?;

    metrics::counter!("rides_started_total").increment(1);
    tracing::info!(
        rental_id = %rental.id,
        scooter_id = %rental.scooter_id,
        from_reservation = req.reservation_id.is_some(),
        hold_amount_kopeks = hold_amount,
        "ride started"
    );
    Ok(rental)
}

/// `GET /api/v1/rides/{id}`: снапшот; стоимость активной поездки считает
/// DTO на текущий момент — ничего не фиксирует. Платёж — из payment-service
/// (hold/captured), недоступность шлюза чек не ломает.
pub async fn snapshot(
    state: &AppState,
    user_id: Uuid,
    ride_id: Uuid,
) -> AppResult<db::rentals::Rental> {
    let rental = db::rentals::find_owned(&state.pool, user_id, ride_id).await?;
    Ok(rental)
}

/// Платёж поездки для DTO чека; сбой шлюза → None (чек без блока оплаты).
pub async fn ride_payment(
    state: &AppState,
    rental_id: Uuid,
) -> Option<crate::services::payments::PaymentRecord> {
    match state.payments.find(rental_id).await {
        Ok(payment) => payment,
        Err(error) => {
            tracing::warn!(%error, rental_id = %rental_id, "payment lookup failed");
            None
        }
    }
}

/// `POST /api/v1/rides/{id}/finish`: лок → расчёт → фиксация.
/// Повторный вызов — тот же `finished` без новых событий (идемпотентно).
#[tracing::instrument(skip_all, fields(user_id = %user_id, rental_id = %ride_id))]
pub async fn finish(
    state: &AppState,
    user_id: Uuid,
    ride_id: Uuid,
    lat: Option<f64>,
    lon: Option<f64>,
) -> AppResult<db::rentals::Rental> {
    let rental = db::rentals::find_owned(&state.pool, user_id, ride_id).await?;
    match rental.status.as_str() {
        // Идемпотентность: повторный финиш возвращает тот же результат.
        "finished" => return Ok(rental),
        "failed" => {
            return Err(AppError::Conflict {
                code: "ride_not_active",
                message: format!("ride {ride_id} is not active"),
            });
        }
        _ => {}
    }

    // Лок не подтвердился → 502, поездка остаётся активной, юзер ретраит.
    crate::services::locks::LockGateway::lock(&state.locks, rental.scooter_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, rental_id = %rental.id, "lock ack timeout");
            AppError::Upstream {
                code: "lock_ack_timeout",
                message: "scooter did not confirm lock".into(),
            }
        })?;

    let finished_at = chrono::Utc::now();
    let cost = state.tariff.cost(rental.started_at, finished_at);
    let data = db::rentals::FinishData {
        finished_at,
        total_min: cost.total_min,
        amount_kopeks: cost.amount_kopeks,
        lat,
        lon,
    };
    match db::rentals::finish(&state.pool, user_id, ride_id, data).await? {
        db::rentals::FinishOutcome::Finished(finished) => {
            // Capture после фиксации финиша (идемпотентный ключ ride:{rental_id},
            // ADR-0014). Эквайринг недоступен — юзера не роняем: capture уйдёт
            // в retry-очередь payment-service (джоб сверки, ADR-0003).
            match state.payments.capture(ride_id, cost.amount_kopeks).await {
                Ok(outcome) => tracing::info!(
                    rental_id = %finished.id,
                    amount_kopeks = cost.amount_kopeks,
                    ?outcome,
                    "ride finished, payment captured"
                ),
                Err(error) => {
                    metrics::counter!("payment_capture_failed_total").increment(1);
                    tracing::warn!(
                        %error,
                        rental_id = %finished.id,
                        amount_kopeks = cost.amount_kopeks,
                        "capture failed after finish; queued for reconciliation"
                    );
                }
            }
            tracing::info!(
                rental_id = %finished.id,
                total_min = cost.total_min,
                amount_kopeks = cost.amount_kopeks,
                "ride finished"
            );
            Ok(finished)
        }
        // Гонку двух финишей закрыл UPDATE..WHERE status='active'.
        db::rentals::FinishOutcome::AlreadyFinished(existing) => Ok(existing),
        db::rentals::FinishOutcome::NotActive => Err(AppError::Conflict {
            code: "ride_not_active",
            message: format!("ride {ride_id} is not active"),
        }),
    }
}

/// `GET /api/v1/rides`: история, свежие сверху; курсор — started_at.
/// Возвращает `(страница, next_before)` — курсор есть, только если страница полная.
pub async fn history(
    state: &AppState,
    user_id: Uuid,
    before: Option<chrono::DateTime<chrono::Utc>>,
    limit: i64,
) -> AppResult<(
    Vec<db::rentals::Rental>,
    Option<chrono::DateTime<chrono::Utc>>,
)> {
    let limit = limit.clamp(1, MAX_HISTORY_LIMIT);
    let (items, has_more) = db::rentals::history(&state.pool, user_id, before, limit).await?;
    let next_before = if has_more {
        items.last().map(|r| r.started_at)
    } else {
        None
    };
    Ok((items, next_before))
}

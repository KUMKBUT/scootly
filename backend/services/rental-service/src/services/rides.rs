//! Поездки (MVP #4): старт из брони или напрямую (unlock), тик стоимости,
//! финиш (лок + расчёт). Схема статусов — openapi `Ride`: active | finished | failed.
//!
//! Оплата (MVP #5, ADR-0003/0014): холд YooKassa на старте (порядок контракта
//! холд → unlock → 201), capture с ключом `ride:{rental_id}` на финише.
//! Компенсация unlock-fail (MVP #6, ADR-0006): попытка unlock пишется как
//! `RideAttempt`; нет ack за 10 c → void холда, поездка `failed`,
//! самокат `offline`, юзер уведомлён (событие `rental.unlock-failed.v1`).

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
    // failed, самокат снова available (ADR-0003).
    let hold_amount = state.tariff.hold_amount();
    if let Err(error) = state.payments.hold(user_id, rental.id, hold_amount).await {
        tracing::warn!(%error, rental_id = %rental.id, "hold rejected, compensating start");
        db::rentals::fail_hold(&state.pool, rental.id).await?;
        return Err(error);
    }

    // Попытка unlock пишется ДО отправки команды (ADR-0006): у каждого старта
    // свой исход — ack или компенсация.
    let attempt =
        match db::ride_attempts::create_pending(&state.pool, rental.id, rental.scooter_id).await {
            Ok(attempt) => Some(attempt),
            Err(error) => {
                // Без строки попытки старт не ломаем: компенсация ниже не зависит от неё.
                tracing::error!(%error, rental_id = %rental.id, "ride_attempt write failed");
                None
            }
        };

    if let Err(error) =
        crate::services::locks::LockGateway::unlock(&state.locks, rental.scooter_id).await
    {
        // Замок не подтвердил unlock за 10 c (502 unlock_timeout): компенсация
        // ADR-0006 — void холда, поездка failed, самокат offline, юзер уведомлён.
        tracing::error!(%error, rental_id = %rental.id, "unlock failed, compensating");
        metrics::counter!("unlock_failed_total").increment(1);
        if let Some(attempt) = attempt {
            if let Err(mark_error) = db::ride_attempts::mark_failed(&state.pool, attempt.id).await {
                tracing::error!(%mark_error, attempt_id = %attempt.id, "attempt fail mark failed");
            }
        }
        compensate_unlock_fail(state, &rental).await;
        return Err(AppError::Upstream {
            code: "unlock_timeout",
            message: "scooter did not confirm unlock".into(),
        });
    }

    if let Some(attempt) = attempt {
        // Ack получен; сбой отметки не роняет начавшуюся поездку.
        if let Err(error) = db::ride_attempts::mark_acked(&state.pool, attempt.id).await {
            tracing::warn!(%error, attempt_id = %attempt.id, "attempt ack mark failed");
        }
    }

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

/// Компенсация unlock-fail (MVP #6, ADR-0006): 1) фиксация failed-состояния в
/// PG (поездка `failed`, самокат `offline`, события `rental.unlock-failed.v1` +
/// `scooter.status.v1` — нотификация и карта по ним придут в Mini App), затем
/// 2) void холда у payment-service (идемпотентно, `hold:{rental_id}`).
/// Каждый шаг — лучший из возможных: сбой одного не отменяет остальные.
async fn compensate_unlock_fail(state: &AppState, rental: &db::rentals::Rental) {
    if let Err(error) = db::rentals::fail_unlock(&state.pool, rental.id).await {
        tracing::error!(%error, rental_id = %rental.id, "unlock-fail state fix failed");
        return;
    }

    match state.payments.void(rental.id).await {
        Ok(outcome) => tracing::info!(
            rental_id = %rental.id,
            ?outcome,
            "hold voided after unlock fail"
        ),
        Err(error) => {
            metrics::counter!("payment_void_failed_total").increment(1);
            // PG держит платёж в `hold` на стороне payment-service — доведёт
            // джоб сверки (`void.retry.v1`), деньги юзера не теряются.
            tracing::error!(
                %error,
                rental_id = %rental.id,
                "hold void failed; queued for reconciliation"
            );
        }
    }
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

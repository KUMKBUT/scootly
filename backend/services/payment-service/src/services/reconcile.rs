//! Джоб сверки (ADR-0003, MVP #11): раз в 5 минут доводит операции, застрявшие
//! из-за недоступности эквайринга, и сверяет состояние PG с YooKassa:
//! 1) retry-очередь outbox `capture.retry.v1` / `void.retry.v1` (retry 3x →
//!    в лог под алерт, DLQ-топик — MVP #8, ADR-0010);
//! 2) холды без движения дольше [`HOLD_STALE_SECS`] — источник истины YooKassa:
//!    succeeded → capture, canceled → снятие (потерянный вебхук не теряет деньги).
//!
//! Рестарт безопасен: очередь в PG, состояние платежа — тоже (PG source of truth).

use std::time::Duration;
use uuid::Uuid;

use db::payments::{self, CaptureOutcome};

use super::yookassa::{YkPayment, YkStatus, YooKassaGateway};
use crate::AppState;

const RETRY_TOPIC: &str = "capture.retry.v1";
/// Очередь void (снятие холда не дошло до эквайринга, MVP #6).
const VOID_TOPIC: &str = "void.retry.v1";
/// Сколько раз джоб повторяет операцию, прежде чем оставить в DLQ (ADR-0010).
pub const MAX_ATTEMPTS: u32 = 3;
const BATCH: i64 = 100;
/// Холд без движения дольше этого возраста сверяется с YooKassa (MVP #11).
/// Активная поездка дольше порога просто лишний раз подтверждает
/// `waiting_for_capture` — состояние не меняется.
pub const HOLD_STALE_SECS: i64 = 15 * 60;
const SYNC_BATCH: i64 = 200;

/// Цикл джоба: интервал из env `RECONCILIATION_INTERVAL_SECS` (по умолчанию 5 мин).
pub async fn run(state: AppState, interval: Duration) {
    loop {
        match reconcile_once(&state).await {
            Ok(processed) if processed > 0 => {
                tracing::info!(processed, "reconcile pass finished");
            }
            Ok(_) => {}
            Err(error) => tracing::error!(%error, "reconcile pass failed"),
        }
        tokio::time::sleep(interval).await;
    }
}

/// Один проход (retry-очереди + сверка с YooKassa): число разобранных записей.
#[tracing::instrument(skip_all)]
pub async fn reconcile_once(state: &AppState) -> common::AppResult<usize> {
    let mut processed = reconcile_captures(state).await?;
    processed += reconcile_voids(state).await?;
    processed += sync_holds_with_gateway(state).await?;
    Ok(processed)
}

/// Доводит capture: `hold → captured` на финальную сумму.
async fn reconcile_captures(state: &AppState) -> common::AppResult<usize> {
    let queue = db::outbox::unpublished_by_topic(&state.pool, RETRY_TOPIC, BATCH).await?;
    let mut processed = 0;

    for record in queue {
        let mut payload = record.payload.clone();
        let rental_id = payload
            .get("rental_id")
            .and_then(|v| v.as_str())
            .and_then(|v| Uuid::parse_str(v).ok());
        let yookassa_id = payload
            .get("yookassa_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned();
        let amount = payload
            .get("amount_kopeks")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32;
        let attempts = payload
            .get("attempts")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;

        let Some(rental_id) = rental_id else {
            tracing::error!(record_id = %record.id, "capture.retry.v1 without rental_id");
            db::outbox::mark_published(&state.pool, record.id).await?;
            continue;
        };

        // Сначала сверяемся с PG: capture мог уже дойти до финиша.
        let payment = match payments::find_by_rental(&state.pool, rental_id).await? {
            Some(p) => p,
            None => {
                tracing::warn!(%rental_id, "retry for unknown payment, dropping");
                db::outbox::mark_published(&state.pool, record.id).await?;
                continue;
            }
        };
        if payment.status == "captured" {
            db::outbox::mark_published(&state.pool, record.id).await?;
            processed += 1;
            continue;
        }

        if attempts >= MAX_ATTEMPTS {
            // Дальше только ручной разбор: DLQ-топик заведёт релей outbox (MVP #8).
            tracing::error!(
                payment_id = %payment.id,
                attempts,
                "capture exhausted retries, needs manual DLQ handling"
            );
            continue;
        }

        match state
            .yookassa
            .capture(&yookassa_id, amount, &super::yookassa::ride_key(rental_id))
            .await
        {
            Ok(_) => match payments::capture(&state.pool, rental_id, amount).await? {
                CaptureOutcome::Captured(_) | CaptureOutcome::AlreadyCaptured(_) => {
                    db::outbox::mark_published(&state.pool, record.id).await?;
                    metrics::counter!("payment_captures_total").increment(1);
                    tracing::info!(payment_id = %payment.id, "reconcile captured payment");
                }
                CaptureOutcome::Canceled(p) => {
                    // Холд отменён, пока capture был в очереди, — снимать нечего.
                    tracing::info!(payment_id = %p.id, "reconcile skipped canceled hold");
                    db::outbox::mark_published(&state.pool, record.id).await?;
                }
                CaptureOutcome::NotFound => {
                    db::outbox::mark_published(&state.pool, record.id).await?;
                }
            },
            Err(error) => {
                payload["attempts"] = serde_json::json!(attempts + 1);
                db::outbox::set_payload(&state.pool, record.id, &payload).await?;
                tracing::warn!(
                    %error,
                    payment_id = %payment.id,
                    attempts = attempts + 1,
                    "reconcile capture failed"
                );
            }
        }
        processed += 1;
    }
    Ok(processed)
}

/// Доводит void: `hold → canceled`, если снятие холда не дошло до эквайринга
/// (unlock-fail, MVP #6). Capture уже прошёл / холд отменён — запись закрывается.
async fn reconcile_voids(state: &AppState) -> common::AppResult<usize> {
    let queue = db::outbox::unpublished_by_topic(&state.pool, VOID_TOPIC, BATCH).await?;
    let mut processed = 0;

    for record in queue {
        let payload = record.payload.clone();
        let rental_id = payload
            .get("rental_id")
            .and_then(|v| v.as_str())
            .and_then(|v| Uuid::parse_str(v).ok());
        let yookassa_id = payload
            .get("yookassa_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned();
        let attempts = payload
            .get("attempts")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;

        let Some(rental_id) = rental_id else {
            tracing::error!(record_id = %record.id, "void.retry.v1 without rental_id");
            db::outbox::mark_published(&state.pool, record.id).await?;
            continue;
        };

        // Сверяемся с PG: холд могли уже снять (или capture прошёл).
        let payment = match payments::find_by_rental(&state.pool, rental_id).await? {
            Some(p) => p,
            None => {
                tracing::warn!(%rental_id, "void for unknown payment, dropping");
                db::outbox::mark_published(&state.pool, record.id).await?;
                continue;
            }
        };
        if payment.status != "hold" {
            db::outbox::mark_published(&state.pool, record.id).await?;
            processed += 1;
            continue;
        }

        if attempts >= MAX_ATTEMPTS {
            // Дальше только ручной разбор: DLQ-топик заведёт релей outbox (MVP #8).
            tracing::error!(
                payment_id = %payment.id,
                attempts,
                "void exhausted retries, needs manual DLQ handling"
            );
            continue;
        }

        match state.yookassa.cancel_hold(&yookassa_id).await {
            Ok(_) => match payments::cancel_hold(&state.pool, rental_id).await? {
                Some(p) => {
                    db::outbox::mark_published(&state.pool, record.id).await?;
                    metrics::counter!("payment_voids_total").increment(1);
                    tracing::info!(payment_id = %p.id, "reconcile canceled hold");
                }
                None => {
                    db::outbox::mark_published(&state.pool, record.id).await?;
                }
            },
            Err(error) => {
                let mut payload = payload;
                payload["attempts"] = serde_json::json!(attempts + 1);
                db::outbox::set_payload(&state.pool, record.id, &payload).await?;
                tracing::warn!(
                    %error,
                    payment_id = %payment.id,
                    attempts = attempts + 1,
                    "reconcile void failed"
                );
            }
        }
        processed += 1;
    }
    Ok(processed)
}

/// Сверка с YooKassa (MVP #11, ADR-0003: раз в 5 минут): застрявшие холды
/// сверяются с эквайрингом. Источник истины — YooKassa: `succeeded` → capture
/// в PG (вебхук потерялся), `canceled` → снятие холда, остальное — норма
/// (`waiting_for_capture` у активной поездки) или ждёт следующего прохода.
async fn sync_holds_with_gateway(state: &AppState) -> common::AppResult<usize> {
    let stale = payments::stale_holds(&state.pool, HOLD_STALE_SECS, SYNC_BATCH).await?;
    let mut applied = 0;

    for payment in stale {
        match state.yookassa.get(&payment.yookassa_id).await {
            Ok(yk) => {
                applied += apply_gateway_state(state, &payment, &yk).await?;
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    payment_id = %payment.id,
                    yookassa_id = %payment.yookassa_id,
                    "reconcile get failed, will retry next pass"
                );
            }
        }
    }
    Ok(applied)
}

/// Приводит PG к подтверждённому эквайрингом состоянию; 1 — если применили.
async fn apply_gateway_state(
    state: &AppState,
    payment: &payments::Payment,
    yk: &YkPayment,
) -> common::AppResult<usize> {
    let Some(rental_id) = payment.rental_id else {
        tracing::warn!(payment_id = %payment.id, "stale hold without ride, skipping sync");
        return Ok(0);
    };

    match yk.status {
        YkStatus::Succeeded => {
            let amount = if yk.amount_kopeks > 0 {
                yk.amount_kopeks
            } else {
                payment.amount_kopeks
            };
            match payments::capture(&state.pool, rental_id, amount).await? {
                CaptureOutcome::Captured(p) => {
                    metrics::counter!("payment_captures_total").increment(1);
                    metrics::counter!("payment_sync_applied_total").increment(1);
                    tracing::info!(
                        payment_id = %p.id,
                        amount_kopeks = amount,
                        "reconcile synced lost capture from YooKassa"
                    );
                    Ok(1)
                }
                _ => Ok(0),
            }
        }
        YkStatus::Canceled => {
            if let Some(p) = payments::cancel_hold(&state.pool, rental_id).await? {
                metrics::counter!("payment_voids_total").increment(1);
                metrics::counter!("payment_sync_applied_total").increment(1);
                tracing::info!(payment_id = %p.id, "reconcile synced canceled hold from YooKassa");
                return Ok(1);
            }
            Ok(0)
        }
        // waiting_for_capture / pending — норма для живого холда; неизвестное —
        // ждём вебхук или следующий проход.
        YkStatus::WaitingForCapture | YkStatus::Pending | YkStatus::Unknown => Ok(0),
    }
}

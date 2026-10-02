//! Бизнес-логика платежей (MVP #5): холд, capture, вебхук, методы оплаты,
//! джоб сверки (ADR-0003, ADR-0014).

use common::{AppError, AppResult};
use db::payments::{self, CaptureOutcome, Payment};
use uuid::Uuid;

use super::yookassa::{YkStatus, YooKassa, YooKassaGateway};
use crate::AppState;

/// Холд на старте поездки: запись `payments` со статусом `hold`, ключ
/// `ride:{rental_id}` (одна запись на поездку — двойной холд невозможен).
#[tracing::instrument(skip_all, fields(user_id = %user_id, rental_id = %rental_id))]
pub async fn hold(
    state: &AppState,
    user_id: Uuid,
    rental_id: Uuid,
    hold_amount_kopeks: i32,
) -> AppResult<Payment> {
    let yk = state
        .yookassa
        .create_hold(&super::yookassa::hold_key(rental_id), hold_amount_kopeks)
        .await?;
    let payment = payments::create_hold(
        &state.pool,
        user_id,
        rental_id,
        &yk.yookassa_id,
        hold_amount_kopeks,
    )
    .await?;
    metrics::counter!("payment_holds_total").increment(1);
    tracing::info!(
        payment_id = %payment.id,
        yookassa_id = %payment.yookassa_id,
        hold_amount_kopeks,
        "hold created"
    );
    Ok(payment)
}

/// Итог capture для gRPC-ответа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureResult {
    /// Списано сейчас.
    Captured,
    /// Уже списано раньше (повторный финиш/вебхук) — без второго списания.
    AlreadyCaptured,
    /// Эквайринг недоступен: capture ушёл в retry-очередь джоба сверки.
    QueuedForRetry,
}

/// Capture на финише. Сбой шлюза НЕ отдаёт ошибку наверх (ADR-0014): платёж
/// остаётся `hold`, retry-запись ложится в outbox, джоб сверки доводит.
#[tracing::instrument(skip_all, fields(rental_id = %rental_id))]
pub async fn capture(
    state: &AppState,
    rental_id: Uuid,
    amount_kopeks: i32,
) -> AppResult<CaptureResult> {
    let payment = payments::find_by_rental(&state.pool, rental_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("payment for rental {rental_id}")))?;

    if payment.status == "captured" {
        return Ok(CaptureResult::AlreadyCaptured);
    }

    let outcome = match state
        .yookassa
        .capture(
            &payment.yookassa_id,
            amount_kopeks,
            &super::yookassa::ride_key(rental_id),
        )
        .await
    {
        // Эквайринг принял capture (succeeded / pending — доведёт сверка).
        Ok(yk) if matches!(yk.status, YkStatus::Succeeded | YkStatus::Pending) => {
            match payments::capture(&state.pool, rental_id, amount_kopeks).await? {
                CaptureOutcome::Captured(p) => {
                    metrics::counter!("payment_captures_total").increment(1);
                    tracing::info!(
                        payment_id = %p.id,
                        amount_kopeks,
                        "payment captured"
                    );
                    CaptureResult::Captured
                }
                // Повторный capture: строка одна, второго списания нет по построению.
                CaptureOutcome::AlreadyCaptured(_) => CaptureResult::AlreadyCaptured,
                CaptureOutcome::Canceled(_) => {
                    return Err(AppError::Conflict {
                        code: "capture_failed",
                        message: format!("hold for rental {rental_id} is canceled"),
                    });
                }
                CaptureOutcome::NotFound => {
                    return Err(AppError::NotFound(format!(
                        "payment for rental {rental_id}"
                    )));
                }
            }
        }
        // Эквайринг ответил, но capture не подтверждён (canceled / unknown):
        // истину установит джоб сверки, повтор — из retry-очереди.
        Ok(yk) => {
            metrics::counter!("payment_captures_retry_total").increment(1);
            tracing::warn!(
                status = ?yk.status,
                rental_id = %rental_id,
                "capture not confirmed by acquiring, queued for retry"
            );
            queue_capture_retry(state, &payment, amount_kopeks, 0).await?;
            CaptureResult::QueuedForRetry
        }
        Err(error) => {
            // ADR-0003/0014: не роняем финиш — очередь на повтор в outbox
            // (retry 3x backoff → DLQ, ADR-0010; релей в Kafka — MVP #8).
            // Отказ/чужой статус от эквайринга разбирает джоб сверки.
            metrics::counter!("payment_captures_retry_total").increment(1);
            tracing::warn!(%error, rental_id = %rental_id, "capture failed, queued for retry");
            queue_capture_retry(state, &payment, amount_kopeks, 0).await?;
            CaptureResult::QueuedForRetry
        }
    };
    Ok(outcome)
}

async fn queue_capture_retry(
    state: &AppState,
    payment: &Payment,
    amount_kopeks: i32,
    attempts: u32,
) -> AppResult<()> {
    db::outbox::push_pool(
        &state.pool,
        "capture.retry.v1",
        &serde_json::json!({
            "payment_id": payment.id,
            "rental_id": payment.rental_id,
            "yookassa_id": payment.yookassa_id,
            "amount_kopeks": amount_kopeks,
            "attempts": attempts,
        }),
    )
    .await
}

/// Вебхук YooKassa (openapi paymentWebhook, без JWT, MVP #11): подлинность —
/// повторным запросом состояния платежа в шлюз; обработка идемпотентна по
/// статусу в PG. Наверх всегда 200 (openapi): разбор — асинхронный, ошибки
/// только в логах.
///
/// ADR-0003 #4: вебхуки приходят не по порядку — действуем по верифицированному
/// статусу эквайринга, а не по порядку доставки. Шлюз недоступен — молча ждём
/// ретрай вебхука от YooKassa.
#[tracing::instrument(skip_all, fields(event = %notification.event, yookassa_id = %notification.object.id))]
pub async fn apply_webhook(state: &AppState, notification: &crate::dto::WebhookNotification) {
    let payment = match payments::find_by_yookassa_id(&state.pool, &notification.object.id).await {
        Ok(Some(payment)) => payment,
        Ok(None) => {
            tracing::warn!("webhook for unknown payment");
            return;
        }
        Err(error) => {
            tracing::error!(%error, "webhook lookup failed");
            return;
        }
    };

    // Верификация: доверяем только состоянию платежа в YooKassa.
    let verified = match state.yookassa.get(&notification.object.id).await {
        Ok(yk) => yk,
        Err(error) => {
            tracing::warn!(%error, "webhook verification failed, waiting for retry");
            return;
        }
    };

    // Неизвестный статус (эмуляция) — верим полю `event` уведомления.
    let event = match verified.status {
        YkStatus::Succeeded => "payment.succeeded".to_owned(),
        YkStatus::Canceled => "payment.canceled".to_owned(),
        // Холд подтверждён/ждёт — в нашей модели это тот же `hold`, делать нечего.
        YkStatus::WaitingForCapture | YkStatus::Pending => return,
        YkStatus::Unknown => notification.event.clone(),
    };

    match event.as_str() {
        "payment.waiting_for_capture" => {}
        // Списание подтверждено: держим PG в согласии, даже если наш capture
        // ещё не дошёл (джоб сверки уже не будет спорить с эквайрингом).
        "payment.succeeded" => match payment.rental_id {
            Some(rental_id) => {
                match payments::capture(&state.pool, rental_id, payment.amount_kopeks).await {
                    Ok(CaptureOutcome::Captured(p)) => {
                        metrics::counter!("payment_captures_total").increment(1);
                        tracing::info!(payment_id = %p.id, "webhook captured payment");
                    }
                    Ok(_) => {}
                    Err(error) => tracing::error!(%error, "webhook capture failed"),
                }
            }
            None => tracing::warn!(payment_id = %payment.id, "payment without ride"),
        },
        // Холд отменён (истёк / отклонён): `hold → canceled` (schema-mvp §4).
        "payment.canceled" => match payment.rental_id {
            Some(rental_id) => match payments::cancel_hold(&state.pool, rental_id).await {
                Ok(Some(p)) => tracing::info!(payment_id = %p.id, "webhook canceled payment"),
                Ok(None) => {}
                Err(error) => tracing::error!(%error, "webhook cancel failed"),
            },
            None => tracing::warn!(payment_id = %payment.id, "payment without ride"),
        },
        // Возвраты — после MVP (schema-mvp §4: refunded из поддержки).
        "payment.refunded" => {
            tracing::info!(payment_id = %payment.id, "refund notification ignored (post-MVP)")
        }
        other => tracing::warn!(event = %other, "unknown webhook event"),
    }
}

/// `GET /api/v1/payments`: история платежей юзера.
pub async fn history(
    state: &AppState,
    user_id: Uuid,
    ride_id: Option<Uuid>,
    limit: i64,
) -> AppResult<Vec<Payment>> {
    payments::history(
        &state.pool,
        user_id,
        ride_id,
        limit.clamp(1, crate::MAX_HISTORY_LIMIT),
    )
    .await
}

/// Привязка карты (MVP — максимум одна): платёж-привязка в YooKassa,
/// клиент открывает `confirmation_url` (openapi createPaymentMethod).
/// URL отдаёт шлюз: настоящий — из ответа API, эмуляция — «на столе».
pub async fn bind_method(state: &AppState, user_id: Uuid) -> AppResult<String> {
    let yk = state
        .yookassa
        .create_hold(&format!("bind:{user_id}"), 100)
        .await?;
    tracing::info!(user_id = %user_id, yookassa_id = %yk.yookassa_id, "card binding payment created");
    Ok(yk.confirmation_url.unwrap_or_else(|| {
        format!(
            "https://yoomoney.ru/checkout/payments/v2/contract?yk_id={}",
            yk.yookassa_id
        )
    }))
}

/// Привязанные карты (MVP — максимум одна). В эмуляции — тестовая карта;
/// с настоящим шлюзом сохранённые карты появятся вместе с хранением
/// `payment_method_id` (после MVP) — пока список пуст.
pub fn list_methods(yookassa: &YooKassa) -> Vec<crate::dto::PaymentMethodDto> {
    match yookassa {
        YooKassa::Emulated => vec![crate::dto::PaymentMethodDto {
            id: "emulated-card".to_owned(),
            card_last4: "4444".to_owned(),
            card_network: "mir".to_owned(),
        }],
        YooKassa::Http(_) | YooKassa::Failing => Vec::new(),
    }
}

/// Итог void холда (unlock-fail, ADR-0006).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelResult {
    /// Холд снят сейчас (`hold → canceled`, событие в outbox).
    Canceled,
    /// Холда уже нет (повторный void / платежа не было) — делать нечего.
    AlreadyCanceled,
    /// Эквайринг недоступен: void ушёл в retry-очередь джоба сверки.
    QueuedForRetry,
}

/// Компенсация unlock-fail (MVP #6, ADR-0006): снятие холда, идемпотентно по
/// `hold:{rental_id}`. Эквайринг недоступен — холд не теряем: retry-запись
/// в outbox (`void.retry.v1`), джоб сверки доводит (как capture, ADR-0003/0014).
#[tracing::instrument(skip_all, fields(rental_id = %rental_id))]
pub async fn cancel_hold(state: &AppState, rental_id: Uuid) -> AppResult<CancelResult> {
    let Some(payment) = payments::find_by_rental(&state.pool, rental_id).await? else {
        return Ok(CancelResult::AlreadyCanceled);
    };
    if payment.status != "hold" {
        return Ok(CancelResult::AlreadyCanceled);
    }

    match state.yookassa.cancel_hold(&payment.yookassa_id).await {
        Ok(_) => match payments::cancel_hold(&state.pool, rental_id).await? {
            Some(p) => {
                metrics::counter!("payment_voids_total").increment(1);
                tracing::info!(payment_id = %p.id, yookassa_id = %p.yookassa_id, "hold canceled (void)");
                Ok(CancelResult::Canceled)
            }
            None => Ok(CancelResult::AlreadyCanceled),
        },
        Err(error) => {
            metrics::counter!("payment_voids_retry_total").increment(1);
            tracing::warn!(
                %error,
                payment_id = %payment.id,
                "void failed at acquiring, queued for retry"
            );
            queue_void_retry(state, &payment, 0).await?;
            Ok(CancelResult::QueuedForRetry)
        }
    }
}

async fn queue_void_retry(state: &AppState, payment: &Payment, attempts: u32) -> AppResult<()> {
    db::outbox::push_pool(
        &state.pool,
        "void.retry.v1",
        &serde_json::json!({
            "payment_id": payment.id,
            "rental_id": payment.rental_id,
            "yookassa_id": payment.yookassa_id,
            "attempts": attempts,
        }),
    )
    .await
}

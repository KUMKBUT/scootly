//! Репозиторий `payments` (оплата, MVP #5; ADR-0003, ADR-0014). Никаких прямых
//! SQL-запросов вне crates/db.
//!
//! Одна запись на поездку: создаётся на холде с ключом `ride:{rental_id}`
//! (UNIQUE), статус `hold → captured` на финише. Двойное списание невозможно
//! по построению: строка одна + переход `UPDATE .. WHERE status='hold'`,
//! повторный capture её не меняет. Событие `payment.events.v1` пишется в
//! outbox в одной транзакции с изменением статуса.

use common::{AppError, AppResult};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

/// Ключ холда (ADR-0014): детерминированно выводится, в БД не хранится.
pub fn hold_key(rental_id: Uuid) -> String {
    format!("hold:{rental_id}")
}

/// Ключ capture (ADR-0014): хранится в `payments.idempotency_key`.
pub fn ride_key(rental_id: Uuid) -> String {
    format!("ride:{rental_id}")
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Payment {
    pub id: Uuid,
    pub user_id: Uuid,
    pub rental_id: Option<Uuid>,
    pub yookassa_id: String,
    /// Для `hold` — сумма холда, после capture — финальная сумма поездки.
    pub amount_kopeks: i32,
    /// `hold | captured | canceled | refunded` (openapi `Payment.status`).
    pub status: String,
    pub idempotency_key: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

const PAYMENT_COLUMNS: &str =
    "id, user_id, rental_id, yookassa_id, amount_kopeks, status, idempotency_key, created_at";

fn internal(error: sqlx::Error) -> AppError {
    AppError::Internal(error.into())
}

/// Итог capture: различие нужно, чтобы повторный финиш/вебхук не плодили события.
#[derive(Debug)]
pub enum CaptureOutcome {
    /// `hold → captured`, событие `payment.events.v1` опубликовано в outbox.
    Captured(Payment),
    /// Уже `captured`: состояние не менялось, событий нет (идемпотентно).
    AlreadyCaptured(Payment),
    /// Холд был отменён (unlock-fail / юзер не стартовал) — capture невозможен.
    Canceled(Payment),
    /// Записи о платеже нет (холд не создавался).
    NotFound,
}

/// Холд: одна запись на поездку, идемпотентно по `ride:{rental_id}`.
/// Повторный вызов (ретрай старта) возвращает существующую запись без дублей.
pub async fn create_hold(
    pool: &PgPool,
    user_id: Uuid,
    rental_id: Uuid,
    yookassa_id: &str,
    amount_kopeks: i32,
) -> AppResult<Payment> {
    let inserted = sqlx::query_as::<_, Payment>(&format!(
        r#"
        INSERT INTO payments (user_id, rental_id, yookassa_id, amount_kopeks, status, idempotency_key)
        VALUES ($1, $2, $3, $4, 'hold', $5)
        ON CONFLICT (idempotency_key) DO NOTHING
        RETURNING {PAYMENT_COLUMNS}
        "#
    ))
    .bind(user_id)
    .bind(rental_id)
    .bind(yookassa_id)
    .bind(amount_kopeks)
    .bind(ride_key(rental_id))
    .fetch_optional(pool)
    .await
    .map_err(internal)?;

    if let Some(payment) = inserted {
        return Ok(payment);
    }
    // Повторный холд: ключ тот же — возвращаем существующую запись.
    find_by_rental(pool, rental_id)
        .await?
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("hold conflict without row")))
}

/// Capture: `hold → captured`, сумма фиксируется финальная.
/// Идемпотентен: `UPDATE .. WHERE status='hold'` — повторный вызов не меняет
/// состояние и не публикует событие.
pub async fn capture(
    pool: &PgPool,
    rental_id: Uuid,
    amount_kopeks: i32,
) -> AppResult<CaptureOutcome> {
    let mut tx = pool.begin().await.map_err(internal)?;

    let captured = sqlx::query_as::<_, Payment>(&format!(
        r#"
        UPDATE payments
        SET status = 'captured', amount_kopeks = $2
        WHERE rental_id = $1 AND status = 'hold'
        RETURNING {PAYMENT_COLUMNS}
        "#
    ))
    .bind(rental_id)
    .bind(amount_kopeks)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal)?;

    let Some(payment) = captured else {
        let existing = find_by_rental_tx(&mut tx, rental_id).await?;
        tx.commit().await.map_err(internal)?;
        return Ok(match existing {
            Some(p) if p.status == "captured" => CaptureOutcome::AlreadyCaptured(p),
            Some(p) => CaptureOutcome::Canceled(p),
            None => CaptureOutcome::NotFound,
        });
    };

    super::outbox::push(
        &mut tx,
        "payment.events.v1",
        &serde_json::json!({
            "event": "payment.succeeded",
            "payment_id": payment.id,
            "rental_id": rental_id,
            "user_id": payment.user_id,
            "amount": amount_kopeks,
        }),
    )
    .await?;

    tx.commit().await.map_err(internal)?;
    Ok(CaptureOutcome::Captured(payment))
}

/// Снятие холда (unlock-fail MVP #6 / отмена в YooKassa): `hold → canceled`.
/// Идемпотентен: `UPDATE .. WHERE status='hold'` — повторный вызов ничего
/// не меняет и не публикует событие. Событие `payment.events.v1`
/// (`payment.canceled`) пишется в outbox в одной транзакции со сменой статуса.
pub async fn cancel_hold(pool: &PgPool, rental_id: Uuid) -> AppResult<Option<Payment>> {
    let mut tx = pool.begin().await.map_err(internal)?;

    let canceled = sqlx::query_as::<_, Payment>(&format!(
        r#"
        UPDATE payments
        SET status = 'canceled'
        WHERE rental_id = $1 AND status = 'hold'
        RETURNING {PAYMENT_COLUMNS}
        "#
    ))
    .bind(rental_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal)?;

    match canceled {
        Some(payment) => {
            super::outbox::push(
                &mut tx,
                "payment.events.v1",
                &serde_json::json!({
                    "event": "payment.canceled",
                    "payment_id": payment.id,
                    "rental_id": rental_id,
                    "user_id": payment.user_id,
                    "amount": payment.amount_kopeks,
                }),
            )
            .await?;
            tx.commit().await.map_err(internal)?;
            Ok(Some(payment))
        }
        None => {
            let existing = find_by_rental_tx(&mut tx, rental_id).await?;
            tx.commit().await.map_err(internal)?;
            Ok(existing)
        }
    }
}

pub async fn find_by_rental(pool: &PgPool, rental_id: Uuid) -> AppResult<Option<Payment>> {
    sqlx::query_as::<_, Payment>(&format!(
        "SELECT {PAYMENT_COLUMNS} FROM payments WHERE rental_id = $1"
    ))
    .bind(rental_id)
    .fetch_optional(pool)
    .await
    .map_err(internal)
}

/// Поиск по id платежа в YooKassa (вебхук приходит с ним, openapi paymentWebhook).
pub async fn find_by_yookassa_id(pool: &PgPool, yookassa_id: &str) -> AppResult<Option<Payment>> {
    sqlx::query_as::<_, Payment>(&format!(
        "SELECT {PAYMENT_COLUMNS} FROM payments WHERE yookassa_id = $1"
    ))
    .bind(yookassa_id)
    .fetch_optional(pool)
    .await
    .map_err(internal)
}

/// История платежей юзера (openapi listPayments): свежие сверху,
/// фильтр по поездке, курсорный `limit`.
pub async fn history(
    pool: &PgPool,
    user_id: Uuid,
    ride_id: Option<Uuid>,
    limit: i64,
) -> AppResult<Vec<Payment>> {
    sqlx::query_as::<_, Payment>(&format!(
        r#"
        SELECT {PAYMENT_COLUMNS}
        FROM payments
        WHERE user_id = $1 AND ($2::uuid IS NULL OR rental_id = $2)
        ORDER BY created_at DESC, id DESC
        LIMIT $3
        "#
    ))
    .bind(user_id)
    .bind(ride_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(internal)
}

/// Чистка за собой в тестах (юзеры остаются — как в auth-контракте).
pub async fn delete_by_user_ids(pool: &PgPool, user_ids: &[Uuid]) -> AppResult<u64> {
    let result = sqlx::query("DELETE FROM payments WHERE user_id = ANY($1)")
        .bind(user_ids)
        .execute(pool)
        .await
        .map_err(internal)?;
    Ok(result.rows_affected())
}

async fn find_by_rental_tx(
    tx: &mut sqlx::PgConnection,
    rental_id: Uuid,
) -> AppResult<Option<Payment>> {
    sqlx::query_as::<_, Payment>(&format!(
        "SELECT {PAYMENT_COLUMNS} FROM payments WHERE rental_id = $1"
    ))
    .bind(rental_id)
    .fetch_optional(tx)
    .await
    .map_err(internal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idempotency_keys_follow_adr0014() {
        let id = Uuid::new_v4();
        assert_eq!(hold_key(id), format!("hold:{id}"));
        assert_eq!(ride_key(id), format!("ride:{id}"));
    }
}

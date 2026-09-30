//! Репозиторий `bookings` (брони, MVP #3). Никаких прямых SQL-запросов вне crates/db.
//!
//! Бронирование без race (ADR-0003, ADR-0015, docs/mvp.md §5.1):
//!   1. Самокат берётся атомарно — `UPDATE scooters SET status='booked'
//!      WHERE id=$1 AND status='available' RETURNING ...`, никаких SELECT-then-UPDATE.
//!   2. Вторая линия обороны — частичные UNIQUE (`uq_active_booking_per_scooter`,
//!      `uq_active_booking_per_user`, миграция 0002).
//!   3. Бронь + outbox пишутся в одной транзакции: сбой откатывает всё —
//!      самокат остаётся `available`, записей нет.
//!
//! PostgreSQL — source of truth; Redis — только TTL-триггер, расхождение
//! закрывает фоновый джоб сверки ([`sweep_expired`]).

use common::{AppError, AppResult};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Booking {
    pub id: Uuid,
    pub user_id: Uuid,
    pub scooter_id: Uuid,
    pub status: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Строка джоба сверки: бронь снята по TTL, самокат возвращён в выдачу.
#[derive(Debug, Clone, Serialize)]
pub struct ExpiredBooking {
    pub booking: Booking,
    /// false — самокат уже не `booked` (например, успел уехать в аренду).
    pub scooter_released: bool,
}

fn internal(error: sqlx::Error) -> AppError {
    AppError::Internal(error.into())
}

/// Мапит нарушение частичных UNIQUE в доменные 409 (docs/api/openapi.yaml).
fn conflict_from_insert(error: sqlx::Error) -> AppError {
    let Some(database) = error.as_database_error() else {
        return AppError::Internal(error.into());
    };
    match database.constraint() {
        Some("uq_active_booking_per_user") => AppError::Conflict {
            code: "reservation_active_exists",
            message: "user already has an active reservation".into(),
        },
        Some("uq_active_booking_per_scooter") => AppError::Conflict {
            code: "scooter_unavailable",
            message: "scooter is no longer available".into(),
        },
        _ => AppError::Internal(error.into()),
    }
}

/// Создаёт бронь атомарно: захват самоката + insert + outbox — одна транзакция.
/// `ttl` может быть отрицательным (тесты TTL-кейса).
pub async fn create(
    pool: &PgPool,
    user_id: Uuid,
    scooter_id: Uuid,
    ttl: chrono::Duration,
) -> AppResult<Booking> {
    let mut tx = pool.begin().await.map_err(internal)?;

    // Лимит: не больше одной активной брони на юзера (§5.1). Проверяем до захвата
    // самоката, чтобы повторный POST (тот же юзер + тот же самокат) давал понятный
    // `reservation_active_exists`, а не гонку двух UNIQUE-нарушений. Гонку двух
    // параллельных броней одного юзера закрывает сам `uq_active_booking_per_user`.
    let user_has_active: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM bookings WHERE user_id = $1 AND status = 'active')",
    )
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(internal)?;
    if user_has_active {
        return Err(AppError::Conflict {
            code: "reservation_active_exists",
            message: "user already has an active reservation".into(),
        });
    }

    let grabbed = sqlx::query_scalar::<_, Uuid>(
        "UPDATE scooters SET status = 'booked'
         WHERE id = $1 AND status = 'available'
         RETURNING id",
    )
    .bind(scooter_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal)?;

    if grabbed.is_none() {
        // Откат транзакции вернёт всё на место: ни брони, ни outbox-записи.
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM scooters WHERE id = $1)")
                .bind(scooter_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(internal)?;
        return Err(if exists {
            AppError::Conflict {
                code: "scooter_unavailable",
                message: format!("scooter {scooter_id} is not available"),
            }
        } else {
            AppError::NotFound(format!("scooter {scooter_id}"))
        });
    }

    let expires_at = chrono::Utc::now() + ttl;
    let booking = sqlx::query_as::<_, Booking>(
        r#"
        INSERT INTO bookings (user_id, scooter_id, status, expires_at)
        VALUES ($1, $2, 'active', $3)
        RETURNING id, user_id, scooter_id, status, expires_at, created_at
        "#,
    )
    .bind(user_id)
    .bind(scooter_id)
    .bind(expires_at)
    .fetch_one(&mut *tx)
    .await
    .map_err(conflict_from_insert)?;

    // Outbox — в той же транзакции (ADR-0008): событие не потеряется и не задублируется.
    super::outbox::push(
        &mut tx,
        "booking.created.v1",
        &serde_json::json!({
            "booking_id": booking.id,
            "scooter_id": booking.scooter_id,
            "user_id": booking.user_id,
            "expires_at": booking.expires_at,
        }),
    )
    .await?;
    super::outbox::push(
        &mut tx,
        "scooter.status.v1",
        &serde_json::json!({ "scooter_id": booking.scooter_id, "status": "booked" }),
    )
    .await?;

    tx.commit().await.map_err(internal)?;
    Ok(booking)
}

/// Итог ручной отмены: 204 в обоих случаях, различие — для событий.
#[derive(Debug)]
pub enum CancelOutcome {
    /// Активная бронь отменена; `scooter_released` — самокат вернулся в `available`.
    Canceled { scooter_released: bool },
    /// Бронь уже в терминальном состоянии (идемпотентность DELETE).
    AlreadyTerminal,
}

/// Отмена брони владельцем. Чужая/несуществующая бронь — 404 (openapi).
pub async fn cancel(pool: &PgPool, user_id: Uuid, booking_id: Uuid) -> AppResult<CancelOutcome> {
    let mut tx = pool.begin().await.map_err(internal)?;

    let booking = sqlx::query_as::<_, Booking>(
        r#"
        UPDATE bookings SET status = 'canceled'
        WHERE id = $1 AND user_id = $2 AND status = 'active'
        RETURNING id, user_id, scooter_id, status, expires_at, created_at
        "#,
    )
    .bind(booking_id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal)?;

    let Some(booking) = booking else {
        // Нет такой брони у юзера или она уже в терминальном состоянии.
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM bookings WHERE id = $1 AND user_id = $2")
                .bind(booking_id)
                .bind(user_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(internal)?;
        return match status {
            Some(_) => Ok(CancelOutcome::AlreadyTerminal),
            None => Err(AppError::NotFound(format!("reservation {booking_id}"))),
        };
    };

    // Самокат возвращаем, только если он всё ещё `booked` (гонка со стартом аренды).
    let released = sqlx::query_scalar::<_, Uuid>(
        "UPDATE scooters SET status = 'available'
         WHERE id = $1 AND status = 'booked'
         RETURNING id",
    )
    .bind(booking.scooter_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal)?
    .is_some();

    super::outbox::push(
        &mut tx,
        "booking.expired.v1",
        &serde_json::json!({
            "booking_id": booking.id,
            "scooter_id": booking.scooter_id,
            "user_id": booking.user_id,
            "reason": "canceled",
        }),
    )
    .await?;
    if released {
        super::outbox::push(
            &mut tx,
            "scooter.status.v1",
            &serde_json::json!({ "scooter_id": booking.scooter_id, "status": "available" }),
        )
        .await?;
    }

    tx.commit().await.map_err(internal)?;
    Ok(CancelOutcome::Canceled {
        scooter_released: released,
    })
}

/// Один проход джоба сверки (ADR-0003): истёкшие активные брони → `expired`,
/// их самокаты (если всё ещё `booked`) → `available`, outbox — в той же транзакции.
pub async fn sweep_expired(pool: &PgPool) -> AppResult<Vec<ExpiredBooking>> {
    let mut tx = pool.begin().await.map_err(internal)?;

    let expired = sqlx::query_as::<_, Booking>(
        r#"
        UPDATE bookings SET status = 'expired'
        WHERE status = 'active' AND expires_at <= now()
        RETURNING id, user_id, scooter_id, status, expires_at, created_at
        "#,
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(internal)?;

    if expired.is_empty() {
        tx.commit().await.map_err(internal)?;
        return Ok(Vec::new());
    }

    let scooter_ids: Vec<Uuid> = expired.iter().map(|b| b.scooter_id).collect();
    let released: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE scooters SET status = 'available'
         WHERE id = ANY($1) AND status = 'booked'
         RETURNING id",
    )
    .bind(&scooter_ids)
    .fetch_all(&mut *tx)
    .await
    .map_err(internal)?;

    for booking in &expired {
        super::outbox::push(
            &mut tx,
            "booking.expired.v1",
            &serde_json::json!({
                "booking_id": booking.id,
                "scooter_id": booking.scooter_id,
                "user_id": booking.user_id,
                "reason": "ttl",
            }),
        )
        .await?;
    }
    for scooter_id in &released {
        super::outbox::push(
            &mut tx,
            "scooter.status.v1",
            &serde_json::json!({ "scooter_id": scooter_id, "status": "available" }),
        )
        .await?;
    }

    tx.commit().await.map_err(internal)?;
    let released_set: std::collections::HashSet<Uuid> = released.into_iter().collect();
    Ok(expired
        .into_iter()
        .map(|booking| {
            let scooter_released = released_set.contains(&booking.scooter_id);
            ExpiredBooking {
                booking,
                scooter_released,
            }
        })
        .collect())
}

pub async fn find_by_id(pool: &PgPool, id: Uuid) -> AppResult<Booking> {
    sqlx::query_as::<_, Booking>(
        "SELECT id, user_id, scooter_id, status, expires_at, created_at FROM bookings WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(|e| match e {
        sqlx::Error::RowNotFound => AppError::NotFound(format!("reservation {id}")),
        other => AppError::Internal(other.into()),
    })
}

/// Чистка за собой в тестах (users остаются — как в auth-контракте).
pub async fn delete_by_user_ids(pool: &PgPool, user_ids: &[Uuid]) -> AppResult<u64> {
    let result = sqlx::query("DELETE FROM bookings WHERE user_id = ANY($1)")
        .bind(user_ids)
        .execute(pool)
        .await
        .map_err(internal)?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_codes_match_openapi() {
        let unavailable = AppError::Conflict {
            code: "scooter_unavailable",
            message: "x".into(),
        };
        assert_eq!(unavailable.code(), "scooter_unavailable");
        let exists = AppError::Conflict {
            code: "reservation_active_exists",
            message: "x".into(),
        };
        assert_eq!(exists.code(), "reservation_active_exists");
    }
}

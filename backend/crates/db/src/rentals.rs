//! Репозиторий `rentals` (поездки, MVP #4). Никаких прямых SQL-запросов вне crates/db.
//!
//! Без race, как у броней (ADR-0003):
//!   1. Самокат переводится в `rented` атомарно — `UPDATE scooters SET status='rented'
//!      WHERE id=$1 AND status IN (...) RETURNING id`, никаких SELECT-then-UPDATE.
//!   2. Вторая линия — частичный UNIQUE `uq_active_ride_per_user` (миграция 0004):
//!      не больше одной активной поездки на юзера (`409 ride_in_progress`).
//!   3. Поездка + outbox — одна транзакция: сбой откатывает всё.
//!
//! Финиш идемпотентен: `UPDATE .. WHERE status='active' RETURNING` — повторный
//! вызов не меняет состояние и не публикует события (ADR-0014, без двойного
//! capture: ключ `ride:{rental_id}` ставит payment-service, MVP #5).

use common::{AppError, AppResult};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Rental {
    pub id: Uuid,
    pub user_id: Uuid,
    pub scooter_id: Uuid,
    pub reservation_id: Option<Uuid>,
    pub tariff: String,
    /// `active | finished | failed` (openapi `Ride.status`).
    pub status: String,
    pub hold_id: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    pub total_min: Option<i32>,
    pub amount_kopeks: Option<i32>,
    pub finished_lat: Option<f64>,
    pub finished_lon: Option<f64>,
}

const RENTAL_COLUMNS: &str = "id, user_id, scooter_id, reservation_id, tariff, status, hold_id, \
     started_at, finished_at, total_min, amount_kopeks, finished_lat, finished_lon";

fn internal(error: sqlx::Error) -> AppError {
    AppError::Internal(error.into())
}

/// Мапит нарушение `uq_active_ride_per_user` в 409 (docs/api/openapi.yaml).
fn conflict_from_insert(error: sqlx::Error) -> AppError {
    match error
        .as_database_error()
        .map(|e| e.constraint())
        .unwrap_or_default()
    {
        Some("uq_active_ride_per_user") => AppError::Conflict {
            code: "ride_in_progress",
            message: "user already has an active ride".into(),
        },
        _ => AppError::Internal(error.into()),
    }
}

fn ride_started_payload(rental: &Rental) -> serde_json::Value {
    serde_json::json!({
        "rental_id": rental.id,
        "scooter_id": rental.scooter_id,
        "user_id": rental.user_id,
        "started_at": rental.started_at,
        "reservation_id": rental.reservation_id,
    })
}

/// Прямой старт: самокат `available` → `rented`, бронь не участвует.
/// `rental_id` генерирует сервис — из него выводится ключ холда (ADR-0014).
pub async fn create_direct(
    pool: &PgPool,
    user_id: Uuid,
    scooter_id: Uuid,
    rental_id: Uuid,
    hold_id: &str,
) -> AppResult<Rental> {
    let mut tx = pool.begin().await.map_err(internal)?;

    let active_ride: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM rentals WHERE user_id = $1 AND status = 'active')",
    )
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(internal)?;
    if active_ride {
        return Err(AppError::Conflict {
            code: "ride_in_progress",
            message: "user already has an active ride".into(),
        });
    }

    grab_scooter(&mut tx, scooter_id, "available").await?;

    let rental = sqlx::query_as::<_, Rental>(&format!(
        r#"
        INSERT INTO rentals (id, user_id, scooter_id, tariff, status, hold_id)
        VALUES ($4, $1, $2, 'per_minute', 'active', $3)
        RETURNING {RENTAL_COLUMNS}
        "#
    ))
    .bind(user_id)
    .bind(scooter_id)
    .bind(hold_id)
    .bind(rental_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(conflict_from_insert)?;

    publish_started(&mut tx, &rental).await?;
    tx.commit().await.map_err(internal)?;
    Ok(rental)
}

/// Старт из брони: бронь `active` → `converted`, самокат `booked` → `rented`.
pub async fn create_from_reservation(
    pool: &PgPool,
    user_id: Uuid,
    reservation_id: Uuid,
    rental_id: Uuid,
    hold_id: &str,
) -> AppResult<Rental> {
    let mut tx = pool.begin().await.map_err(internal)?;

    let booking = sqlx::query_as::<_, super::bookings::Booking>(
        "UPDATE bookings SET status = 'converted'
         WHERE id = $1 AND user_id = $2 AND status = 'active'
         RETURNING id, user_id, scooter_id, status, expires_at, created_at",
    )
    .bind(reservation_id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal)?;
    let Some(booking) = booking else {
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM bookings WHERE id = $1 AND user_id = $2")
                .bind(reservation_id)
                .bind(user_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(internal)?;
        return Err(match status {
            // Бронь уже в терминальном состоянии — по контракту она «истекла».
            Some(_) => AppError::Conflict {
                code: "reservation_expired",
                message: format!("reservation {reservation_id} is not active"),
            },
            None => AppError::NotFound(format!("reservation {reservation_id}")),
        });
    };

    // Лимит: не больше одной активной поездки на юзера. После конвертации —
    // сбой ниже откатит и её (бронь останется active).
    let active_ride: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM rentals WHERE user_id = $1 AND status = 'active')",
    )
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(internal)?;
    if active_ride {
        return Err(AppError::Conflict {
            code: "ride_in_progress",
            message: "user already has an active ride".into(),
        });
    }

    grab_scooter(&mut tx, booking.scooter_id, "booked").await?;

    let rental = sqlx::query_as::<_, Rental>(&format!(
        r#"
        INSERT INTO rentals (id, user_id, scooter_id, reservation_id, tariff, status, hold_id)
        VALUES ($5, $1, $2, $3, 'per_minute', 'active', $4)
        RETURNING {RENTAL_COLUMNS}
        "#
    ))
    .bind(user_id)
    .bind(booking.scooter_id)
    .bind(reservation_id)
    .bind(hold_id)
    .bind(rental_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(conflict_from_insert)?;

    // Конвертация — терминальный исход брони (asyncapi BookingExpired.reason=converted).
    super::outbox::push(
        &mut tx,
        "booking.expired.v1",
        &serde_json::json!({
            "booking_id": booking.id,
            "scooter_id": booking.scooter_id,
            "user_id": booking.user_id,
            "reason": "converted",
        }),
    )
    .await?;
    publish_started(&mut tx, &rental).await?;

    tx.commit().await.map_err(internal)?;
    Ok(rental)
}

/// Итог финиша: различие нужно сервису для идемпотентного 200.
#[derive(Debug)]
pub enum FinishOutcome {
    /// Активная поездка завершена; события опубликованы.
    Finished(Rental),
    /// Повторный финиш: состояние не менялось, события не публиковались.
    AlreadyFinished(Rental),
    /// Поездка в статусе `failed` → 409 ride_not_active.
    NotActive,
}

/// Результат расчёта, который сервис передаёт на фиксацию.
#[derive(Debug, Clone, Copy)]
pub struct FinishData {
    pub finished_at: chrono::DateTime<chrono::Utc>,
    pub total_min: i32,
    pub amount_kopeks: i32,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

/// Финиш владельцем: `active` → `finished`, самокат `rented` → `available`.
/// Сумму считает сервис (tariff), репозиторий только фиксирует результат.
pub async fn finish(
    pool: &PgPool,
    user_id: Uuid,
    rental_id: Uuid,
    data: FinishData,
) -> AppResult<FinishOutcome> {
    let mut tx = pool.begin().await.map_err(internal)?;

    let rental = sqlx::query_as::<_, Rental>(&format!(
        r#"
        UPDATE rentals
        SET status = 'finished', finished_at = $3, total_min = $4, amount_kopeks = $5,
            finished_lat = $6, finished_lon = $7
        WHERE id = $1 AND user_id = $2 AND status = 'active'
        RETURNING {RENTAL_COLUMNS}
        "#
    ))
    .bind(rental_id)
    .bind(user_id)
    .bind(data.finished_at)
    .bind(data.total_min)
    .bind(data.amount_kopeks)
    .bind(data.lat)
    .bind(data.lon)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal)?;

    let Some(rental) = rental else {
        return Ok(match find_owned_tx(&mut tx, user_id, rental_id).await? {
            Some(existing) if existing.status == "finished" => {
                FinishOutcome::AlreadyFinished(existing)
            }
            Some(_) => FinishOutcome::NotActive,
            None => {
                return Err(AppError::NotFound(format!("ride {rental_id}")));
            }
        });
    };

    let released = sqlx::query_scalar::<_, Uuid>(
        "UPDATE scooters SET status = 'available'
         WHERE id = $1 AND status = 'rented'
         RETURNING id",
    )
    .bind(rental.scooter_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal)?
    .is_some();

    super::outbox::push(
        &mut tx,
        "rental.finished.v1",
        &serde_json::json!({
            "rental_id": rental.id,
            "scooter_id": rental.scooter_id,
            "user_id": rental.user_id,
            "total_min": rental.total_min,
            "amount_kopeks": rental.amount_kopeks,
        }),
    )
    .await?;
    if released {
        super::outbox::push(
            &mut tx,
            "scooter.status.v1",
            &serde_json::json!({ "scooter_id": rental.scooter_id, "status": "available" }),
        )
        .await?;
    }

    tx.commit().await.map_err(internal)?;
    Ok(FinishOutcome::Finished(rental))
}

/// Компенсация неудачного холда (MVP #5, ADR-0003): старт не состоялся —
/// поездка `failed` (момент фиксируется в finished_at), самокат `rented →
/// available`. Вызывается, когда замок ещё не получал unlock (холд отклонён).
pub async fn fail_hold(pool: &PgPool, rental_id: Uuid) -> AppResult<Option<Rental>> {
    let mut tx = pool.begin().await.map_err(internal)?;

    let rental = sqlx::query_as::<_, Rental>(&format!(
        r#"
        UPDATE rentals
        SET status = 'failed', finished_at = now(), total_min = 0, amount_kopeks = 0
        WHERE id = $1 AND status = 'active'
        RETURNING {RENTAL_COLUMNS}
        "#
    ))
    .bind(rental_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal)?;

    let Some(rental) = rental else {
        tx.commit().await.map_err(internal)?;
        return Ok(None);
    };

    let released = sqlx::query_scalar::<_, Uuid>(
        "UPDATE scooters SET status = 'available'
         WHERE id = $1 AND status = 'rented'
         RETURNING id",
    )
    .bind(rental.scooter_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal)?
    .is_some();

    if released {
        super::outbox::push(
            &mut tx,
            "scooter.status.v1",
            &serde_json::json!({ "scooter_id": rental.scooter_id, "status": "available" }),
        )
        .await?;
    }

    tx.commit().await.map_err(internal)?;
    tracing::warn!(rental_id = %rental.id, "ride failed before unlock (hold rejected)");
    Ok(Some(rental))
}

/// Компенсация unlock-fail (MVP #6, ADR-0006): замок не подтвердил unlock за
/// 10 c — поездка `failed` (`amount_kopeks = 0`), самокат `rented → offline`
/// (устройство «под вопросом», в выдачу не возвращается), в outbox — событие
/// `rental.unlock-failed.v1` (нотификация юзеру) и статус самоката.
/// Идемпотентно: `UPDATE .. WHERE status='active'` — повторный вызов ничего
/// не меняет и не публикует события. Void холда делает payment-service
/// (ключ `hold:{rental_id}`), сюда не тянется.
pub async fn fail_unlock(pool: &PgPool, rental_id: Uuid) -> AppResult<Option<Rental>> {
    let mut tx = pool.begin().await.map_err(internal)?;

    let rental = sqlx::query_as::<_, Rental>(&format!(
        r#"
        UPDATE rentals
        SET status = 'failed', finished_at = now(), total_min = 0, amount_kopeks = 0
        WHERE id = $1 AND status = 'active'
        RETURNING {RENTAL_COLUMNS}
        "#
    ))
    .bind(rental_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal)?;

    let Some(rental) = rental else {
        tx.commit().await.map_err(internal)?;
        return Ok(None);
    };

    let offline = sqlx::query_scalar::<_, Uuid>(
        "UPDATE scooters SET status = 'offline'
         WHERE id = $1 AND status = 'rented'
         RETURNING id",
    )
    .bind(rental.scooter_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal)?
    .is_some();

    super::outbox::push(
        &mut tx,
        "rental.unlock-failed.v1",
        &serde_json::json!({
            "rental_id": rental.id,
            "scooter_id": rental.scooter_id,
            "user_id": rental.user_id,
            "reason": "lock_ack_timeout",
        }),
    )
    .await?;
    if offline {
        super::outbox::push(
            &mut tx,
            "scooter.status.v1",
            &serde_json::json!({ "scooter_id": rental.scooter_id, "status": "offline" }),
        )
        .await?;
    }

    tx.commit().await.map_err(internal)?;
    tracing::warn!(
        rental_id = %rental.id,
        scooter_id = %rental.scooter_id,
        "ride failed: unlock not acked in 10s, scooter offline"
    );
    Ok(Some(rental))
}

pub async fn find_owned(pool: &PgPool, user_id: Uuid, rental_id: Uuid) -> AppResult<Rental> {
    sqlx::query_as::<_, Rental>(&format!(
        "SELECT {RENTAL_COLUMNS} FROM rentals WHERE id = $1 AND user_id = $2"
    ))
    .bind(rental_id)
    .bind(user_id)
    .fetch_one(pool)
    .await
    .map_err(|e| match e {
        sqlx::Error::RowNotFound => AppError::NotFound(format!("ride {rental_id}")),
        other => AppError::Internal(other.into()),
    })
}

/// История поездок юзера, свежие сверху; `before` — курсор по started_at.
/// Берёт `limit + 1` строку и возвращает `(страница, has_more)` — курсор
/// `next_before` сервис ставит только когда есть следующая страница.
pub async fn history(
    pool: &PgPool,
    user_id: Uuid,
    before: Option<chrono::DateTime<chrono::Utc>>,
    limit: i64,
) -> AppResult<(Vec<Rental>, bool)> {
    let rows = sqlx::query_as::<_, Rental>(&format!(
        r#"
        SELECT {RENTAL_COLUMNS}
        FROM rentals
        WHERE user_id = $1 AND ($2::timestamptz IS NULL OR started_at < $2)
        ORDER BY started_at DESC, id DESC
        LIMIT $3
        "#
    ))
    .bind(user_id)
    .bind(before)
    .bind(limit + 1)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    let has_more = rows.len() as i64 > limit;
    let page = rows.into_iter().take(limit as usize).collect();
    Ok((page, has_more))
}

/// Чистка за собой в тестах (users остаются — как в auth-контракте).
pub async fn delete_by_user_ids(pool: &PgPool, user_ids: &[Uuid]) -> AppResult<u64> {
    let result = sqlx::query("DELETE FROM rentals WHERE user_id = ANY($1)")
        .bind(user_ids)
        .execute(pool)
        .await
        .map_err(internal)?;
    Ok(result.rows_affected())
}

/// Атомарный захват самоката под поездку: только из ожидаемого статуса.
async fn grab_scooter(
    tx: &mut sqlx::PgConnection,
    scooter_id: Uuid,
    from_status: &str,
) -> AppResult<()> {
    let grabbed = sqlx::query_scalar::<_, Uuid>(
        "UPDATE scooters SET status = 'rented'
         WHERE id = $1 AND status = $2
         RETURNING id",
    )
    .bind(scooter_id)
    .bind(from_status)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal)?;

    if grabbed.is_none() {
        // Откат транзакции вернёт всё на место: ни поездки, ни outbox-записи.
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM scooters WHERE id = $1)")
                .bind(scooter_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(internal)?;
        return Err(if exists {
            AppError::Conflict {
                code: "scooter_unavailable",
                message: format!("scooter {scooter_id} is not {from_status}"),
            }
        } else {
            AppError::NotFound(format!("scooter {scooter_id}"))
        });
    }
    Ok(())
}

async fn publish_started(tx: &mut sqlx::PgConnection, rental: &Rental) -> AppResult<()> {
    super::outbox::push(tx, "rental.started.v1", &ride_started_payload(rental)).await?;
    super::outbox::push(
        tx,
        "scooter.status.v1",
        &serde_json::json!({ "scooter_id": rental.scooter_id, "status": "rented" }),
    )
    .await
}

async fn find_owned_tx(
    tx: &mut sqlx::PgConnection,
    user_id: Uuid,
    rental_id: Uuid,
) -> AppResult<Option<Rental>> {
    sqlx::query_as::<_, Rental>(&format!(
        "SELECT {RENTAL_COLUMNS} FROM rentals WHERE id = $1 AND user_id = $2"
    ))
    .bind(rental_id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| AppError::Internal(e.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_codes_match_openapi() {
        let in_progress = AppError::Conflict {
            code: "ride_in_progress",
            message: "x".into(),
        };
        assert_eq!(in_progress.code(), "ride_in_progress");
        let expired = AppError::Conflict {
            code: "reservation_expired",
            message: "x".into(),
        };
        assert_eq!(expired.code(), "reservation_expired");
    }
}

//! TTL-триггеры броней (ADR-0003, ADR-0015): `SET booking:ttl:{id} PX <ttl>`
//! при создании брони, `DEL` при ручной отмене/снятии.
//!
//! PG — source of truth: ключ — best-effort подсказка, её потеря не страшна —
//! расхождение закрывает фоновый джоб сверки rental-service (docs/mvp.md §5.1
//! «Redis недоступен»).

use redis::aio::ConnectionManager;
use uuid::Uuid;

pub const TRIGGER_PREFIX: &str = "booking:ttl:";

pub fn trigger_key(booking_id: Uuid) -> String {
    format!("{TRIGGER_PREFIX}{booking_id}")
}

/// Ставит TTL-триггер брони. Ошибки обрабатывает вызывающий (best-effort).
pub async fn arm(
    conn: &mut ConnectionManager,
    booking_id: Uuid,
    ttl: std::time::Duration,
) -> anyhow::Result<()> {
    redis::cmd("SET")
        .arg(trigger_key(booking_id))
        .arg(1)
        .arg("PX")
        .arg(ttl.as_millis() as i64)
        .query_async::<_, ()>(conn)
        .await?;
    Ok(())
}

/// Снимает триггер (ручная отмена или джоб сверки).
pub async fn disarm(conn: &mut ConnectionManager, booking_id: Uuid) -> anyhow::Result<()> {
    redis::cmd("DEL")
        .arg(trigger_key(booking_id))
        .query_async::<_, ()>(conn)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_key_format() {
        let id = Uuid::new_v4();
        assert_eq!(trigger_key(id), format!("booking:ttl:{id}"));
    }
}

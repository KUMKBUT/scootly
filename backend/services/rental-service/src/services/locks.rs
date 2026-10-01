//! Шлюз замков: команды unlock/lock (ADR-0002, MQTT — вне MVP).
//!
//! IoT-часть эмулируется (тег `Emulated` в workspace.dsl). Таймаут ack —
//! часть контракта шлюза (MVP #6, ADR-0006): ответа нет за
//! [`UNLOCK_ACK_TIMEOUT_SECS`] — ошибка `unlock_timeout`, сервис выполняет
//! компенсацию (void холда, самокат offline). Варианты [`Locks`]:
//! `Emulated` (всегда ack), `Silent` (не отвечает никогда — тесты компенсации).

use std::time::Duration;

use common::{AppError, AppResult};
use uuid::Uuid;

/// Ожидание ack от замка — 10 секунд (таймаут команды `scooter/{id}/cmd`,
/// ADR-0006; DoD MVP: unlock-fail за 10 c).
pub const UNLOCK_ACK_TIMEOUT_SECS: u64 = 10;

/// Подтверждение замка: ack в течение таймаута.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockAck {
    pub scooter_id: Uuid,
}

/// Точка интеграции с замками. Асинхронный контракт — под MQTT с QoS 1
/// (ADR-0009: дедуп на консюмере).
pub trait LockGateway: Send + Sync {
    fn unlock(
        &self,
        scooter_id: Uuid,
    ) -> impl std::future::Future<Output = AppResult<LockAck>> + Send;

    fn lock(
        &self,
        scooter_id: Uuid,
    ) -> impl std::future::Future<Output = AppResult<LockAck>> + Send;
}

/// Эмуляция: команда доходит всегда (устройство «на столе»).
#[derive(Debug, Clone, Copy, Default)]
pub struct EmulatedLocks;

impl LockGateway for EmulatedLocks {
    async fn unlock(&self, scooter_id: Uuid) -> AppResult<LockAck> {
        tracing::debug!(%scooter_id, "emulated unlock ack");
        Ok(LockAck { scooter_id })
    }

    async fn lock(&self, scooter_id: Uuid) -> AppResult<LockAck> {
        tracing::debug!(%scooter_id, "emulated lock ack");
        Ok(LockAck { scooter_id })
    }
}

/// Замок не отвечает никогда (устройство офлайн/разряжено) — контракт
/// компенсации unlock-fail в тестах (ADR-0006).
#[derive(Debug, Clone, Copy, Default)]
pub struct SilentLocks;

impl LockGateway for SilentLocks {
    async fn unlock(&self, _scooter_id: Uuid) -> AppResult<LockAck> {
        std::future::pending().await
    }

    async fn lock(&self, _scooter_id: Uuid) -> AppResult<LockAck> {
        std::future::pending().await
    }
}

/// Выбранный шлюз замков (copy-enum вместо `Arc<dyn>` — без async-trait).
#[derive(Debug, Clone, Copy, Default)]
pub enum Locks {
    #[default]
    Emulated,
    Silent,
}

impl LockGateway for Locks {
    async fn unlock(&self, scooter_id: Uuid) -> AppResult<LockAck> {
        // Таймаут команды scooter/{id}/cmd — 10 c (ADR-0006).
        let ack = async {
            match self {
                Self::Emulated => EmulatedLocks.unlock(scooter_id).await,
                Self::Silent => SilentLocks.unlock(scooter_id).await,
            }
        };
        match tokio::time::timeout(Duration::from_secs(UNLOCK_ACK_TIMEOUT_SECS), ack).await {
            Ok(result) => result,
            Err(_elapsed) => Err(unlock_timeout(scooter_id)),
        }
    }

    async fn lock(&self, scooter_id: Uuid) -> AppResult<LockAck> {
        // Финиш: ack ждали бы так же; таймаут компенсирует юзер ретраем
        // (поездка остаётся активной, MVP #4).
        let ack = async {
            match self {
                Self::Emulated => EmulatedLocks.lock(scooter_id).await,
                Self::Silent => SilentLocks.lock(scooter_id).await,
            }
        };
        match tokio::time::timeout(Duration::from_secs(UNLOCK_ACK_TIMEOUT_SECS), ack).await {
            Ok(result) => result,
            Err(_elapsed) => Err(AppError::Upstream {
                code: "lock_ack_timeout",
                message: "scooter did not confirm lock".into(),
            }),
        }
    }
}

fn unlock_timeout(scooter_id: Uuid) -> AppError {
    tracing::warn!(%scooter_id, "unlock ack timeout (10s)");
    AppError::Upstream {
        code: "unlock_timeout",
        message: "scooter did not confirm unlock".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0006: нет ack за 10 c → `unlock_timeout` (виртуальное время — мгновенно).
    #[tokio::test(start_paused = true)]
    async fn silent_lock_times_out_after_10s() {
        let started = tokio::time::Instant::now();
        let error = Locks::Silent
            .unlock(Uuid::new_v4())
            .await
            .expect_err("silent lock must not ack");
        assert_eq!(error.code(), "unlock_timeout");
        // tokio paused-время: ровно таймаут, реальных секунд нет.
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(UNLOCK_ACK_TIMEOUT_SECS)
        );
    }

    #[tokio::test]
    async fn emulated_lock_acks_within_timeout() {
        let scooter_id = Uuid::new_v4();
        let ack = Locks::Emulated
            .unlock(scooter_id)
            .await
            .expect("emulated ack");
        assert_eq!(ack.scooter_id, scooter_id);
    }
}

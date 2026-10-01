//! Шлюз замков: команды unlock/lock (ADR-0002, MQTT — вне MVP).
//!
//! IoT-часть эмулируется (тег `Emulated` в workspace.dsl): заглушка всегда
//! подтверждает команду. Сид для MVP #6 (unlock-fail, ADR-0006): настоящая
//! реализация с таймаутом 10 c добавляет вариант в [`Locks`], контракт
//! (502 `unlock_timeout` / `lock_ack_timeout`) уже соблюдён сервисом.

use common::AppResult;
use uuid::Uuid;

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

/// Выбранный шлюз замков (copy-enum вместо `Arc<dyn>` — без async-trait).
#[derive(Debug, Clone, Copy, Default)]
pub enum Locks {
    #[default]
    Emulated,
}

impl LockGateway for Locks {
    async fn unlock(&self, scooter_id: Uuid) -> AppResult<LockAck> {
        match self {
            Self::Emulated => EmulatedLocks.unlock(scooter_id).await,
        }
    }

    async fn lock(&self, scooter_id: Uuid) -> AppResult<LockAck> {
        match self {
            Self::Emulated => EmulatedLocks.lock(scooter_id).await,
        }
    }
}

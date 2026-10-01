//! Шлюз YooKassa (ADR-0003): payment-service — единственная точка интеграции
//! с эквайрингом. IoT-подобная внешняя система — до боевых ключей эмулируется
//! (тег `Emulated` в workspace.dsl): платежи всегда проходят, id детерминированы
//! по ключу идемпотентности. Настоящая реализация (HTTP API YooKassa) добавит
//! вариант в [`YooKassa`] без изменения контракта сервисов.
//!
//! ADR-0014: при недоступности шлюза capture не роняет запрос юзера —
//! неудача уходит в retry-очередь джоба сверки.

use db::payments;
use uuid::Uuid;

/// Платёж в терминах YooKassa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YkPayment {
    pub yookassa_id: String,
}

/// Точка интеграции с эквайрингом.
pub trait YooKassaGateway: Send + Sync {
    /// Холд: замораживает `amount_kopeks`, возвращает id платежа.
    /// Ключ идемпотентности — `hold:{rental_id}` (ADR-0014, в БД не хранится).
    fn create_hold(
        &self,
        idempotency_key: &str,
        amount_kopeks: i32,
    ) -> impl std::future::Future<Output = common::AppResult<YkPayment>> + Send;

    /// Capture холда на финальную сумму (частичный capture допустим).
    fn capture(
        &self,
        yookassa_id: &str,
        amount_kopeks: i32,
        idempotency_key: &str,
    ) -> impl std::future::Future<Output = common::AppResult<()>> + Send;

    /// Снятие холда (unlock-fail, отмена).
    fn cancel_hold(
        &self,
        yookassa_id: &str,
    ) -> impl std::future::Future<Output = common::AppResult<()>> + Send;

    /// Повторный запрос состояния платежа: вебхуки верифицируются данными
    /// YooKassa, а не доверением входящему телу (openapi paymentWebhook).
    fn get(
        &self,
        yookassa_id: &str,
    ) -> impl std::future::Future<Output = common::AppResult<YkPayment>> + Send;
}

/// Эмуляция: платёж доходит всегда, id выводится детерминированно из ключа,
/// поэтому повторный холд даёт тот же `yookassa_id` (UNIQUE в БД не спорит).
#[derive(Debug, Clone, Copy, Default)]
pub struct EmulatedYooKassa;

fn emulated_id(idempotency_key: &str) -> String {
    // Формат id YooKassa — 22–36 символов; детерминизм важнее похожести.
    let rental = idempotency_key.rsplit(':').next().unwrap_or("x");
    match Uuid::parse_str(rental) {
        Ok(id) => format!("emul-{}", id.simple()),
        Err(_) => format!("emul-{}", Uuid::new_v4().simple()),
    }
}

impl YooKassaGateway for EmulatedYooKassa {
    async fn create_hold(
        &self,
        idempotency_key: &str,
        _amount_kopeks: i32,
    ) -> common::AppResult<YkPayment> {
        tracing::debug!(%idempotency_key, "emulated hold created");
        Ok(YkPayment {
            yookassa_id: emulated_id(idempotency_key),
        })
    }

    async fn capture(
        &self,
        yookassa_id: &str,
        amount_kopeks: i32,
        idempotency_key: &str,
    ) -> common::AppResult<()> {
        tracing::debug!(%yookassa_id, amount_kopeks, %idempotency_key, "emulated capture ok");
        Ok(())
    }

    async fn cancel_hold(&self, yookassa_id: &str) -> common::AppResult<()> {
        tracing::debug!(%yookassa_id, "emulated hold canceled");
        Ok(())
    }

    async fn get(&self, yookassa_id: &str) -> common::AppResult<YkPayment> {
        Ok(YkPayment {
            yookassa_id: yookassa_id.to_owned(),
        })
    }
}

/// Всегда отказывающий шлюз — для тестов retry-очереди и 402-контракта.
#[derive(Debug, Clone, Copy, Default)]
pub struct FailingYooKassa;

impl YooKassaGateway for FailingYooKassa {
    async fn create_hold(
        &self,
        _idempotency_key: &str,
        _amount_kopeks: i32,
    ) -> common::AppResult<YkPayment> {
        Err(common::AppError::Upstream {
            code: "hold_failed",
            message: "acquiring is unavailable".into(),
        })
    }

    async fn capture(
        &self,
        _yookassa_id: &str,
        _amount_kopeks: i32,
        _idempotency_key: &str,
    ) -> common::AppResult<()> {
        Err(common::AppError::Upstream {
            code: "capture_failed",
            message: "acquiring is unavailable".into(),
        })
    }

    async fn cancel_hold(&self, _yookassa_id: &str) -> common::AppResult<()> {
        Err(common::AppError::Upstream {
            code: "capture_failed",
            message: "acquiring is unavailable".into(),
        })
    }

    async fn get(&self, _yookassa_id: &str) -> common::AppResult<YkPayment> {
        Err(common::AppError::Upstream {
            code: "capture_failed",
            message: "acquiring is unavailable".into(),
        })
    }
}

/// Выбранный шлюз (copy-enum вместо `Arc<dyn>` — без async-trait).
#[derive(Debug, Clone, Copy, Default)]
pub enum YooKassa {
    #[default]
    Emulated,
    Failing,
}

impl YooKassaGateway for YooKassa {
    async fn create_hold(
        &self,
        idempotency_key: &str,
        amount_kopeks: i32,
    ) -> common::AppResult<YkPayment> {
        match self {
            Self::Emulated => {
                EmulatedYooKassa
                    .create_hold(idempotency_key, amount_kopeks)
                    .await
            }
            Self::Failing => {
                FailingYooKassa
                    .create_hold(idempotency_key, amount_kopeks)
                    .await
            }
        }
    }

    async fn capture(
        &self,
        yookassa_id: &str,
        amount_kopeks: i32,
        idempotency_key: &str,
    ) -> common::AppResult<()> {
        match self {
            Self::Emulated => {
                EmulatedYooKassa
                    .capture(yookassa_id, amount_kopeks, idempotency_key)
                    .await
            }
            Self::Failing => {
                FailingYooKassa
                    .capture(yookassa_id, amount_kopeks, idempotency_key)
                    .await
            }
        }
    }

    async fn cancel_hold(&self, yookassa_id: &str) -> common::AppResult<()> {
        match self {
            Self::Emulated => EmulatedYooKassa.cancel_hold(yookassa_id).await,
            Self::Failing => FailingYooKassa.cancel_hold(yookassa_id).await,
        }
    }

    async fn get(&self, yookassa_id: &str) -> common::AppResult<YkPayment> {
        match self {
            Self::Emulated => EmulatedYooKassa.get(yookassa_id).await,
            Self::Failing => FailingYooKassa.get(yookassa_id).await,
        }
    }
}

/// Ключ холда для вызова шлюза (ADR-0014, см. [`payments::hold_key`]).
pub fn hold_key(rental_id: Uuid) -> String {
    payments::hold_key(rental_id)
}

/// Ключ capture для вызова шлюза (ADR-0014, см. [`payments::ride_key`]).
pub fn ride_key(rental_id: Uuid) -> String {
    payments::ride_key(rental_id)
}

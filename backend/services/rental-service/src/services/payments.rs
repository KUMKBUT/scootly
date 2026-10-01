//! Шлюз оплаты (MVP #5, ADR-0003/0014): холд на старте, capture на финише.
//! Синхронный канал — gRPC `PaymentOrchestrator` (payment-service — единственная
//! точка интеграции с YooKassa). До подъёма payment-service — эмуляция
//! (платёж «на столе»), для тестов 402/компенсации — всегда отказывающий шлюз.
//!
//! ADR-0014: capture не роняет финиш юзера; сбой → retry-очередь на стороне
//! payment-service (джоб сверки), здесь — только лог и метрика.
//! MVP #6 (ADR-0006): void холда при unlock-fail, идемпотентно по
//! `hold:{rental_id}`.

use common::{AppError, AppResult};
use proto::scootly::payment::v1 as pb;
use proto::scootly::payment::v1::payment_orchestrator_client::PaymentOrchestratorClient;
use tonic::transport::Channel;
use tonic::Status;
use uuid::Uuid;

/// Платёж поездки в терминах rental-service (openapi `Ride.payment`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentRecord {
    pub id: Uuid,
    pub rental_id: Uuid,
    /// `hold | captured | canceled | refunded`.
    pub status: String,
    pub amount_kopeks: i64,
}

/// Итог capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureOutcome {
    Captured,
    AlreadyCaptured,
    QueuedForRetry,
}

/// Итог void (unlock-fail, ADR-0006).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoidOutcome {
    /// Холд снят (`payments.status = canceled`).
    Voided,
    /// Холда уже нет (повтор / не было) — компенсировать нечего.
    AlreadyCanceled,
    /// Void не дошёл до эквайринга — retry-очередь payment-service.
    QueuedForRetry,
}

/// Порт оплаты: асинхронный контракт под gRPC.
pub trait PaymentGateway: Send + Sync {
    fn hold(
        &self,
        user_id: Uuid,
        rental_id: Uuid,
        hold_amount_kopeks: i32,
    ) -> impl std::future::Future<Output = AppResult<PaymentRecord>> + Send;

    fn capture(
        &self,
        rental_id: Uuid,
        amount_kopeks: i32,
    ) -> impl std::future::Future<Output = AppResult<CaptureOutcome>> + Send;

    /// Снятие холда (unlock-fail): идемпотентно, ключ `hold:{rental_id}`.
    fn void(
        &self,
        rental_id: Uuid,
    ) -> impl std::future::Future<Output = AppResult<VoidOutcome>> + Send;

    fn find(
        &self,
        rental_id: Uuid,
    ) -> impl std::future::Future<Output = AppResult<Option<PaymentRecord>>> + Send;
}

/// gRPC-клиент payment-service. Отказ холда маппится в 402 (openapi):
/// `no_payment_method` / `hold_failed`.
#[derive(Debug, Clone)]
pub struct GrpcPayments {
    client: PaymentOrchestratorClient<Channel>,
}

impl GrpcPayments {
    /// `url` — например `http://payment-service:9001`; соединение ленивое.
    pub fn new(url: &str) -> AppResult<Self> {
        let channel = Channel::from_shared(url.to_owned())
            .map_err(|e| AppError::Internal(e.into()))?
            .connect_lazy();
        Ok(Self {
            client: PaymentOrchestratorClient::new(channel),
        })
    }
}

fn hold_error(status: Status) -> AppError {
    let message = status.message().to_owned();
    match status.code() {
        // Холд отклонён: нет привязанной карты.
        tonic::Code::FailedPrecondition => AppError::PaymentRequired {
            code: "no_payment_method",
            message,
        },
        // Эквайринг/payment-service недоступен.
        tonic::Code::Unavailable | tonic::Code::DeadlineExceeded => AppError::PaymentRequired {
            code: "hold_failed",
            message,
        },
        _ => AppError::Upstream {
            code: "hold_failed",
            message,
        },
    }
}

fn payment_from_pb(payment: pb::Payment) -> AppResult<PaymentRecord> {
    Ok(PaymentRecord {
        id: Uuid::parse_str(&payment.id).map_err(|e| AppError::Internal(e.into()))?,
        rental_id: Uuid::parse_str(&payment.rental_id).map_err(|e| AppError::Internal(e.into()))?,
        status: payment.status,
        amount_kopeks: payment.amount_kopeks as i64,
    })
}

impl PaymentGateway for GrpcPayments {
    async fn hold(
        &self,
        user_id: Uuid,
        rental_id: Uuid,
        hold_amount_kopeks: i32,
    ) -> AppResult<PaymentRecord> {
        let mut client = self.client.clone();
        let reply = client
            .hold(pb::HoldRequest {
                user_id: user_id.to_string(),
                rental_id: rental_id.to_string(),
                hold_amount_kopeks: i64::from(hold_amount_kopeks) as u64,
            })
            .await
            .map_err(hold_error)?
            .into_inner();
        Ok(PaymentRecord {
            id: Uuid::parse_str(&reply.payment_id).map_err(|e| AppError::Internal(e.into()))?,
            rental_id,
            status: reply.status,
            amount_kopeks: reply.amount_kopeks as i64,
        })
    }

    async fn capture(&self, rental_id: Uuid, amount_kopeks: i32) -> AppResult<CaptureOutcome> {
        let mut client = self.client.clone();
        let reply = client
            .capture(pb::CaptureRequest {
                rental_id: rental_id.to_string(),
                amount_kopeks: i64::from(amount_kopeks) as u64,
            })
            .await
            .map_err(|status| AppError::Upstream {
                code: "capture_failed",
                message: status.message().to_owned(),
            })?
            .into_inner();
        match reply.outcome.as_str() {
            "captured" => Ok(CaptureOutcome::Captured),
            "already_captured" => Ok(CaptureOutcome::AlreadyCaptured),
            _ => Ok(CaptureOutcome::QueuedForRetry),
        }
    }

    async fn void(&self, rental_id: Uuid) -> AppResult<VoidOutcome> {
        let mut client = self.client.clone();
        let reply = client
            .cancel_hold(pb::CancelHoldRequest {
                rental_id: rental_id.to_string(),
            })
            .await
            .map_err(|status| AppError::Upstream {
                code: "void_failed",
                message: status.message().to_owned(),
            })?
            .into_inner();
        match reply.outcome.as_str() {
            "canceled" => Ok(VoidOutcome::Voided),
            "queued_for_retry" => Ok(VoidOutcome::QueuedForRetry),
            // already_canceled / not_found: холда нет — снимать нечего.
            _ => Ok(VoidOutcome::AlreadyCanceled),
        }
    }

    async fn find(&self, rental_id: Uuid) -> AppResult<Option<PaymentRecord>> {
        let mut client = self.client.clone();
        let reply = client
            .rental_payment(pb::RentalPaymentRequest {
                rental_id: rental_id.to_string(),
            })
            .await
            .map_err(|status| AppError::Upstream {
                code: "capture_failed",
                message: status.message().to_owned(),
            })?
            .into_inner();
        match reply.payment {
            Some(payment) => Ok(Some(payment_from_pb(payment)?)),
            None => Ok(None),
        }
    }
}

/// Эмуляция: холд проходит всегда, сумма возвратом равна запрошенной.
#[derive(Debug, Clone, Copy, Default)]
pub struct EmulatedPayments;

impl PaymentGateway for EmulatedPayments {
    async fn hold(
        &self,
        _user_id: Uuid,
        rental_id: Uuid,
        hold_amount_kopeks: i32,
    ) -> AppResult<PaymentRecord> {
        tracing::debug!(%rental_id, hold_amount_kopeks, "emulated payment hold");
        Ok(PaymentRecord {
            id: Uuid::new_v4(),
            rental_id,
            status: "hold".to_owned(),
            amount_kopeks: i64::from(hold_amount_kopeks),
        })
    }

    async fn capture(&self, rental_id: Uuid, _amount_kopeks: i32) -> AppResult<CaptureOutcome> {
        tracing::debug!(%rental_id, "emulated payment capture");
        Ok(CaptureOutcome::Captured)
    }

    async fn void(&self, rental_id: Uuid) -> AppResult<VoidOutcome> {
        tracing::debug!(%rental_id, "emulated payment void");
        Ok(VoidOutcome::Voided)
    }

    async fn find(&self, _rental_id: Uuid) -> AppResult<Option<PaymentRecord>> {
        // В эмуляции чека нет — payment не возвращаем (DTO его просто опустит).
        Ok(None)
    }
}

/// Всегда отказывающий шлюз — контракт 402 и компенсации старта в тестах.
#[derive(Debug, Clone, Copy, Default)]
pub struct FailingPayments;

impl PaymentGateway for FailingPayments {
    async fn hold(
        &self,
        _user_id: Uuid,
        _rental_id: Uuid,
        _hold_amount_kopeks: i32,
    ) -> AppResult<PaymentRecord> {
        Err(AppError::PaymentRequired {
            code: "hold_failed",
            message: "acquiring is unavailable".into(),
        })
    }

    async fn capture(&self, _rental_id: Uuid, _amount_kopeks: i32) -> AppResult<CaptureOutcome> {
        Err(AppError::Upstream {
            code: "capture_failed",
            message: "acquiring is unavailable".into(),
        })
    }

    async fn void(&self, _rental_id: Uuid) -> AppResult<VoidOutcome> {
        Err(AppError::Upstream {
            code: "void_failed",
            message: "acquiring is unavailable".into(),
        })
    }

    async fn find(&self, _rental_id: Uuid) -> AppResult<Option<PaymentRecord>> {
        Ok(None)
    }
}

/// Выбранный шлюз (copy-enum вместо `Arc<dyn>` — без async-trait).
#[derive(Debug, Clone, Default)]
pub enum Payments {
    #[default]
    Emulated,
    Failing,
    Grpc(GrpcPayments),
}

impl Payments {
    /// Из env `PAYMENT_SERVICE_URL` (например `http://payment-service:9001`);
    /// без переменной — эмуляция (локальный стенд без payment-service).
    pub fn from_env() -> Self {
        match std::env::var("PAYMENT_SERVICE_URL") {
            Ok(url) if !url.is_empty() => match GrpcPayments::new(&url) {
                Ok(grpc) => Self::Grpc(grpc),
                Err(error) => {
                    tracing::error!(%error, "bad PAYMENT_SERVICE_URL, falling back to emulated");
                    Self::Emulated
                }
            },
            _ => Self::Emulated,
        }
    }
}

impl PaymentGateway for Payments {
    async fn hold(
        &self,
        user_id: Uuid,
        rental_id: Uuid,
        hold_amount_kopeks: i32,
    ) -> AppResult<PaymentRecord> {
        match self {
            Self::Emulated => {
                EmulatedPayments
                    .hold(user_id, rental_id, hold_amount_kopeks)
                    .await
            }
            Self::Failing => {
                FailingPayments
                    .hold(user_id, rental_id, hold_amount_kopeks)
                    .await
            }
            Self::Grpc(grpc) => grpc.hold(user_id, rental_id, hold_amount_kopeks).await,
        }
    }

    async fn capture(&self, rental_id: Uuid, amount_kopeks: i32) -> AppResult<CaptureOutcome> {
        match self {
            Self::Emulated => EmulatedPayments.capture(rental_id, amount_kopeks).await,
            Self::Failing => FailingPayments.capture(rental_id, amount_kopeks).await,
            Self::Grpc(grpc) => grpc.capture(rental_id, amount_kopeks).await,
        }
    }

    async fn void(&self, rental_id: Uuid) -> AppResult<VoidOutcome> {
        match self {
            Self::Emulated => EmulatedPayments.void(rental_id).await,
            Self::Failing => FailingPayments.void(rental_id).await,
            Self::Grpc(grpc) => grpc.void(rental_id).await,
        }
    }

    async fn find(&self, rental_id: Uuid) -> AppResult<Option<PaymentRecord>> {
        match self {
            Self::Emulated => EmulatedPayments.find(rental_id).await,
            Self::Failing => FailingPayments.find(rental_id).await,
            Self::Grpc(grpc) => grpc.find(rental_id).await,
        }
    }
}

//! gRPC `PaymentOrchestrator` (MVP #5): синхронный канал rental-service →
//! payment-service для холда/capture (ADR-0003). Внутренний канал: наружу
//! не публикуется (k8s NetworkPolicy / без ingress) — как `ScooterPositions`.

use proto::scootly::payment::v1 as pb;
use proto::scootly::payment::v1::payment_orchestrator_server::PaymentOrchestrator;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::{services, AppState};

/// Маппинг доменных ошибок холда в коды gRPC:
/// `no_payment_method` → failed_precondition, `hold_failed` → unavailable.
fn hold_status(error: common::AppError) -> Status {
    match error {
        common::AppError::PaymentRequired { code, message } if code == "no_payment_method" => {
            Status::failed_precondition(format!("{code}: {message}"))
        }
        common::AppError::PaymentRequired { code, message } => {
            Status::unavailable(format!("{code}: {message}"))
        }
        common::AppError::Upstream { code, message } => {
            Status::unavailable(format!("{code}: {message}"))
        }
        other => {
            tracing::error!(error = %other, "hold failed");
            Status::internal("hold failed")
        }
    }
}

#[derive(Clone)]
pub struct PaymentOrchestratorImpl {
    pub state: AppState,
}

#[tonic::async_trait]
impl PaymentOrchestrator for PaymentOrchestratorImpl {
    #[tracing::instrument(skip_all)]
    async fn hold(
        &self,
        request: Request<pb::HoldRequest>,
    ) -> Result<Response<pb::HoldReply>, Status> {
        let req = request.into_inner();
        let user_id = parse_uuid(&req.user_id)?;
        let rental_id = parse_uuid(&req.rental_id)?;
        let amount = i32::try_from(req.hold_amount_kopeks)
            .map_err(|_| Status::invalid_argument("hold_amount_kopeks out of range"))?;

        let payment = services::payments::hold(&self.state, user_id, rental_id, amount)
            .await
            .map_err(hold_status)?;

        Ok(Response::new(pb::HoldReply {
            payment_id: payment.id.to_string(),
            yookassa_id: payment.yookassa_id,
            status: payment.status,
            amount_kopeks: i64::from(payment.amount_kopeks) as u64,
        }))
    }

    #[tracing::instrument(skip_all)]
    async fn capture(
        &self,
        request: Request<pb::CaptureRequest>,
    ) -> Result<Response<pb::CaptureReply>, Status> {
        let req = request.into_inner();
        let rental_id = parse_uuid(&req.rental_id)?;
        let amount = i32::try_from(req.amount_kopeks)
            .map_err(|_| Status::invalid_argument("amount_kopeks out of range"))?;

        // ADR-0014: capture не роняет финиш — при недоступности эквайринга
        // возвращаем queued_for_retry (джоб сверки доведёт, ADR-0003).
        let outcome = match services::payments::capture(&self.state, rental_id, amount).await {
            Ok(o) => o,
            Err(error) => {
                tracing::error!(%error, rental_id = %rental_id, "capture failed");
                return Err(Status::internal("capture failed"));
            }
        };
        let outcome = match outcome {
            services::payments::CaptureResult::Captured => "captured",
            services::payments::CaptureResult::AlreadyCaptured => "already_captured",
            services::payments::CaptureResult::QueuedForRetry => "queued_for_retry",
        };
        Ok(Response::new(pb::CaptureReply {
            outcome: outcome.to_owned(),
        }))
    }

    #[tracing::instrument(skip_all)]
    async fn cancel_hold(
        &self,
        request: Request<pb::CancelHoldRequest>,
    ) -> Result<Response<pb::CancelHoldReply>, Status> {
        let rental_id = parse_uuid(&request.into_inner().rental_id)?;

        // Void — часть компенсации unlock-fail (ADR-0006): недоступность
        // эквайринга не роняет компенсацию — queued_for_retry (джоб сверки).
        let outcome = match services::payments::cancel_hold(&self.state, rental_id).await {
            Ok(o) => o,
            Err(error) => {
                tracing::error!(%error, rental_id = %rental_id, "cancel_hold failed");
                return Err(Status::internal("cancel_hold failed"));
            }
        };
        let (outcome, status) = match outcome {
            services::payments::CancelResult::Canceled => ("canceled", "canceled"),
            services::payments::CancelResult::AlreadyCanceled => ("already_canceled", "canceled"),
            services::payments::CancelResult::QueuedForRetry => ("queued_for_retry", "hold"),
        };
        Ok(Response::new(pb::CancelHoldReply {
            outcome: outcome.to_owned(),
            status: status.to_owned(),
        }))
    }

    #[tracing::instrument(skip_all)]
    async fn rental_payment(
        &self,
        request: Request<pb::RentalPaymentRequest>,
    ) -> Result<Response<pb::RentalPaymentReply>, Status> {
        let rental_id = parse_uuid(&request.into_inner().rental_id)?;
        let payment = db::payments::find_by_rental(&self.state.pool, rental_id)
            .await
            .map_err(|error| {
                tracing::error!(%error, "rental_payment lookup failed");
                Status::internal("payment lookup failed")
            })?;

        Ok(Response::new(pb::RentalPaymentReply {
            payment: payment.map(|p| pb::Payment {
                id: p.id.to_string(),
                rental_id: p.rental_id.map(|id| id.to_string()).unwrap_or_default(),
                status: p.status,
                amount_kopeks: i64::from(p.amount_kopeks) as u64,
                created_at: p.created_at.to_rfc3339(),
            }),
        }))
    }
}

fn parse_uuid(raw: &str) -> Result<Uuid, Status> {
    Uuid::parse_str(raw).map_err(|_| Status::invalid_argument(format!("bad uuid: {raw}")))
}

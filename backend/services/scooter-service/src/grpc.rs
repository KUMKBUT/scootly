//! gRPC `ScooterPositions.LastPositions` — fallback для geo-service (ADR-0011).
//! Внутренний канал: наружу не публикуется (k8s NetworkPolicy / без ingress).

use proto::scootly::scooter::v1 as pb;
use proto::scootly::scooter::v1::scooter_positions_server::ScooterPositions;
use sqlx::PgPool;
use tonic::{Request, Response, Status};
use uuid::Uuid;

pub const MAX_RADIUS_M: u32 = 3000;
pub const MAX_LIMIT: i64 = 200;
const DEFAULT_LIMIT: i64 = 100;

#[derive(Clone)]
pub struct ScooterPositionsImpl {
    pub pool: PgPool,
}

#[tonic::async_trait]
impl ScooterPositions for ScooterPositionsImpl {
    #[tracing::instrument(skip_all)]
    async fn last_positions(
        &self,
        request: Request<pb::LastPositionsRequest>,
    ) -> Result<Response<pb::LastPositionsResponse>, Status> {
        let req = request.into_inner();

        if !(-90.0..=90.0).contains(&req.lat) || !(-180.0..=180.0).contains(&req.lon) {
            return Err(Status::invalid_argument("lat/lon out of range"));
        }
        let radius_m = req.radius_m.clamp(1, MAX_RADIUS_M);
        let limit = if req.limit == 0 {
            DEFAULT_LIMIT
        } else {
            i64::from(req.limit).min(MAX_LIMIT)
        };
        let mut ids = Vec::with_capacity(req.scooter_ids.len());
        for raw in &req.scooter_ids {
            let id = Uuid::parse_str(raw)
                .map_err(|_| Status::invalid_argument(format!("bad scooter id: {raw}")))?;
            ids.push(id);
        }

        let rows = db::scooters::find_nearby(&self.pool, req.lat, req.lon, radius_m, &ids, limit)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "last_positions lookup failed");
                Status::internal("scooters lookup failed")
            })?;

        let positions = rows
            .into_iter()
            .map(|s| pb::ScooterPosition {
                id: s.id.to_string(),
                code: s.code,
                lat: s.lat,
                lon: s.lon,
                status: s.status,
                battery_pct: s.battery_pct.clamp(0, 100) as u32,
                distance_m: s.distance_m,
            })
            .collect();

        Ok(Response::new(pb::LastPositionsResponse { positions }))
    }
}

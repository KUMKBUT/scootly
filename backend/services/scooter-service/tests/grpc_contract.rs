//! Контракт gRPC fallback'а (ADR-0011): ScooterPositions.LastPositions.
//!
//! Прогон: `cargo test --workspace`. Тест, требующий Postgres, помечен
//! `#[ignore]` — запускается после `make up && make migrate`:
//! `cargo test -p scooter-service -- --ignored`.

use scooter_service::grpc::ScooterPositionsImpl;
use scooter_service::AppState;

use proto::scootly::scooter::v1::scooter_positions_server::ScooterPositions;
use proto::scootly::scooter::v1::LastPositionsRequest;
use tonic::Request;

// Almaty, площадь Астаны.
const CENTER_LAT: f64 = 43.2380;
const CENTER_LON: f64 = 76.8890;

fn unique_code(prefix: &str) -> String {
    // scooters.code — VARCHAR(32): хвост uuid обрезаем под лимит.
    let suffix = 32 - prefix.len() - 1;
    format!(
        "{prefix}-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..suffix]
    )
}

async fn test_pool() -> sqlx::PgPool {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL is not set");
    let pool = db::create_pool_lazy(&database_url).expect("pool");
    sqlx::migrate!("../../crates/db/migrations")
        .run(&pool)
        .await
        .expect("migrations");
    pool
}

fn seed(code: &str, lat: f64, lon: f64, status: &str) -> db::scooters::NewScooter {
    db::scooters::NewScooter {
        code: code.to_owned(),
        lat,
        lon,
        status: status.to_owned(),
        battery_pct: 80,
    }
}

/// Fallback по радиусу: ближайшие первыми, offline скрыт, далеко — за радиусом.
#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn last_positions_radius_order_and_filters() {
    let pool = test_pool().await;
    let near = unique_code("GRPC-NEAR");
    let far = unique_code("GRPC-FAR");
    let offline = unique_code("GRPC-OFF");
    let codes = [near.clone(), far.clone(), offline.clone()];

    for (code, (lat, lon), status) in [
        (&near, (CENTER_LAT, CENTER_LON + 0.001), "available"), // ~81 м
        (&far, (CENTER_LAT, CENTER_LON + 0.01), "available"),   // ~810 м
        (&offline, (CENTER_LAT, CENTER_LON + 0.002), "offline"),
    ] {
        db::scooters::upsert_by_code(&pool, &seed(code, lat, lon, status))
            .await
            .expect("upsert");
    }

    let state = AppState { pool: pool.clone() };
    let service = ScooterPositionsImpl {
        pool: state.pool.clone(),
    };
    let response = service
        .last_positions(Request::new(LastPositionsRequest {
            lat: CENTER_LAT,
            lon: CENTER_LON,
            radius_m: 3000,
            scooter_ids: Vec::new(),
            limit: 200,
        }))
        .await
        .expect("rpc");

    let positions = response.into_inner().positions;
    let codes_returned: Vec<&str> = positions.iter().map(|p| p.code.as_str()).collect();
    assert!(codes_returned.contains(&near.as_str()));
    assert!(codes_returned.contains(&far.as_str()));
    assert!(
        !codes_returned.contains(&offline.as_str()),
        "offline must not be served"
    );

    let near_pos = positions.iter().find(|p| p.code == near).unwrap();
    let far_pos = positions.iter().find(|p| p.code == far).unwrap();
    assert!(
        near_pos.distance_m < far_pos.distance_m,
        "nearest first: {} vs {}",
        near_pos.distance_m,
        far_pos.distance_m
    );
    assert!(near_pos.distance_m < 200.0);
    assert_eq!(near_pos.status, "available");
    assert_eq!(near_pos.battery_pct, 80);

    db::scooters::delete_by_codes(&pool, &codes)
        .await
        .expect("cleanup");
}

/// Кейс ADR-0011 «в GEO-кэше нет записи по самокату»: fallback по ids.
#[tokio::test]
#[ignore = "requires live Postgres (make up && make migrate)"]
async fn last_positions_by_ids_ignores_radius() {
    let pool = test_pool().await;
    let target = unique_code("GRPC-BYID");
    let codes = [target.clone()];
    // 2 км от центра — за пределами radius_m=500 в запросе, но id задан явно.
    db::scooters::upsert_by_code(
        &pool,
        &seed(&target, CENTER_LAT + 0.018, CENTER_LON, "available"),
    )
    .await
    .expect("upsert");

    let scooter = db::scooters::find_by_code(&pool, &target)
        .await
        .expect("scooter");

    let service = ScooterPositionsImpl { pool: pool.clone() };
    let response = service
        .last_positions(Request::new(LastPositionsRequest {
            lat: CENTER_LAT,
            lon: CENTER_LON,
            radius_m: 500,
            scooter_ids: vec![scooter.id.to_string()],
            limit: 10,
        }))
        .await
        .expect("rpc");

    let positions = response.into_inner().positions;
    assert_eq!(positions.len(), 1, "explicit id served even beyond radius");
    assert_eq!(positions[0].code, target);

    db::scooters::delete_by_codes(&pool, &codes)
        .await
        .expect("cleanup");
}

/// Невалидные аргументы → invalid_argument.
#[tokio::test]
async fn last_positions_rejects_bad_args() {
    let database_url = "postgres://invalid:invalid@127.0.0.1:1/none";
    let pool = db::create_pool_lazy(database_url).expect("lazy pool");
    let service = ScooterPositionsImpl { pool };

    let status = service
        .last_positions(Request::new(LastPositionsRequest {
            lat: 91.0,
            lon: 0.0,
            radius_m: 500,
            scooter_ids: Vec::new(),
            limit: 0,
        }))
        .await
        .expect_err("must reject lat>90");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    let status = service
        .last_positions(Request::new(LastPositionsRequest {
            lat: 0.0,
            lon: 0.0,
            radius_m: 500,
            scooter_ids: vec!["not-a-uuid".to_owned()],
            limit: 0,
        }))
        .await
        .expect_err("must reject bad uuid");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

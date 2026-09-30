//! Сидер тестовых данных: самокаты вокруг Almaty → PostgreSQL + Redis GEO
//! (write path как у telemetry-service, ADR-0011). Идемпотентен: `make seed`.

use db::scooters::{NewScooter, Scooter};

struct SeedScooter {
    code: &'static str,
    dlat: f64,
    dlon: f64,
    battery_pct: i32,
    status: &'static str,
}

/// Площадь Астаны, Almaty — как в примерах docs/api.
const CENTER_LAT: f64 = 43.238_0;
const CENTER_LON: f64 = 76.889_0;

const SCOOTERS: &[SeedScooter] = &[
    SeedScooter {
        code: "SC-0001",
        dlat: 0.0000,
        dlon: 0.0000,
        battery_pct: 92,
        status: "available",
    },
    SeedScooter {
        code: "SC-0002",
        dlat: 0.0009,
        dlon: 0.0012,
        battery_pct: 87,
        status: "available",
    },
    SeedScooter {
        code: "SC-0003",
        dlat: -0.0013,
        dlon: 0.0007,
        battery_pct: 74,
        status: "available",
    },
    SeedScooter {
        code: "SC-0004",
        dlat: 0.0021,
        dlon: -0.0018,
        battery_pct: 63,
        status: "available",
    },
    SeedScooter {
        code: "SC-0005",
        dlat: -0.0027,
        dlon: -0.0021,
        battery_pct: 58,
        status: "available",
    },
    SeedScooter {
        code: "SC-0006",
        dlat: 0.0036,
        dlon: 0.0029,
        battery_pct: 45,
        status: "available",
    },
    SeedScooter {
        code: "SC-0007",
        dlat: -0.0041,
        dlon: 0.0033,
        battery_pct: 39,
        status: "available",
    },
    SeedScooter {
        code: "SC-0008",
        dlat: 0.0052,
        dlon: -0.0044,
        battery_pct: 81,
        status: "available",
    },
    SeedScooter {
        code: "SC-0009",
        dlat: -0.0058,
        dlon: -0.0049,
        battery_pct: 96,
        status: "available",
    },
    SeedScooter {
        code: "SC-0010",
        dlat: 0.0066,
        dlon: 0.0058,
        battery_pct: 51,
        status: "available",
    },
    // Для карты: чужая бронь тоже видима, но не бронируется (rental-service, MVP #3).
    SeedScooter {
        code: "SC-0011",
        dlat: 0.0004,
        dlon: -0.0003,
        battery_pct: 88,
        status: "booked",
    },
    // Offline в выдачу не попадает.
    SeedScooter {
        code: "SC-0012",
        dlat: 0.0075,
        dlon: 0.0067,
        battery_pct: 12,
        status: "offline",
    },
];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://scootly:scootly@localhost:5432/scootly".into());
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());

    let pool = db::create_pool(&database_url).await?;
    sqlx::migrate!("../../crates/db/migrations")
        .run(&pool)
        .await?;
    let mut redis_conn = redis_client::connect(&redis_url)
        .await?
        .get_connection_manager()
        .await?;

    let mut cached = 0usize;
    for seed in SCOOTERS {
        let new = NewScooter {
            code: seed.code.to_owned(),
            lat: CENTER_LAT + seed.dlat,
            lon: CENTER_LON + seed.dlon,
            status: seed.status.to_owned(),
            battery_pct: seed.battery_pct,
        };
        let scooter: Scooter = db::scooters::upsert_by_code(&pool, &new).await?;
        tracing::info!(code = %scooter.code, status = %scooter.status, battery_pct = scooter.battery_pct, "seeded");

        // GEO-кэш — только для самокатов из выдачи (ADR-0011).
        if scooter.status != "offline" {
            redis_client::geo::upsert(
                &mut redis_conn,
                &redis_client::geo::GeoScooter {
                    id: scooter.id,
                    code: scooter.code.clone(),
                    lat: scooter.lat,
                    lon: scooter.lon,
                    status: scooter.status.clone(),
                    battery_pct: scooter.battery_pct,
                },
            )
            .await?;
            cached += 1;
        }
    }

    tracing::info!(
        total = SCOOTERS.len(),
        in_geo_cache = cached,
        "seeder done (scooters: PG + Redis GEO)"
    );
    Ok(())
}

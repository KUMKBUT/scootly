//! Метрики (MVP #9, ADR-0014): экспортёр Prometheus на порту сервиса
//! (`GET /metrics`, тот же `:9000`, что и API — scrape-цели см.
//! `infra/monitoring/prometheus/prometheus.yml`).

use axum::routing::get;
use axum::Router;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use std::sync::OnceLock;

static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Установка глобального recorder'а + описания бизнес-счётчиков
/// (HELP в `/metrics` — по ним строятся дашборды Grafana). Вызывать один
/// раз на старте сервиса; повторный вызов безопасен.
pub fn install() {
    if HANDLE.get().is_none() {
        match PrometheusBuilder::new().install_recorder() {
            Ok(handle) => {
                let _ = HANDLE.set(handle);
            }
            Err(error) => tracing::warn!(%error, "prometheus recorder install failed"),
        }
    }
    // Бизнес-метрики (ADR-0014): сколько стартов, сколько отказов замка,
    // сколько самокатов ушли в низкий заряд.
    metrics::describe_counter!(
        "rides_started_total",
        "Rides started (hold ok, unlock acked)"
    );
    metrics::describe_counter!(
        "unlock_failed_total",
        "Unlock attempts without lock ack in 10 s (ADR-0006)"
    );
    metrics::describe_counter!(
        "battery_low_total",
        "Scooters that entered the low-battery state"
    );
}

/// `GET /metrics`: рендер счётчиков в текстовом формате Prometheus.
async fn render() -> String {
    HANDLE
        .get()
        .map(PrometheusHandle::render)
        .unwrap_or_default()
}

/// Добавить `/metrics` к роутеру сервиса — рядом с `/health`.
pub fn route(app: Router) -> Router {
    app.route("/metrics", get(render))
}

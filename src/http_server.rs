use anyhow::{Context, Result};
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Router,
};
use tokio::net::TcpListener;

use crate::app::{MonitorState, SharedState};
use crate::utils::METRICS_BIND_ADDR;

// Convert current monitor state into Prometheus exposition text.
fn render_metrics(snapshot: &MonitorState) -> String {
    let mut body = String::new();

    body.push_str("# HELP oracle_last_check_success 1 if last chain check succeeded, else 0\n");
    body.push_str("# TYPE oracle_last_check_success gauge\n");
    body.push_str(&format!(
        "oracle_last_check_success {}\n",
        if snapshot.last_check_ok { 1 } else { 0 }
    ));

    body.push_str("# HELP oracle_last_check_timestamp_seconds Unix timestamp of last poll attempt\n");
    body.push_str("# TYPE oracle_last_check_timestamp_seconds gauge\n");
    body.push_str(&format!(
        "oracle_last_check_timestamp_seconds {}\n",
        snapshot.last_check_ts
    ));

    body.push_str("# HELP oracle_allowlist_size Number of oracle addresses in latest on-chain allowlist snapshot\n");
    body.push_str("# TYPE oracle_allowlist_size gauge\n");
    body.push_str(&format!("oracle_allowlist_size {}\n", snapshot.per_oracle.len()));

    body.push_str("# HELP oracle_report_present 1 if report was found for oracle in current scan window\n");
    body.push_str("# TYPE oracle_report_present gauge\n");

    body.push_str("# HELP oracle_last_report_timestamp_seconds Unix timestamp of latest matched report tx per oracle\n");
    body.push_str("# TYPE oracle_last_report_timestamp_seconds gauge\n");

    body.push_str("# HELP oracle_time_since_last_report_seconds Seconds elapsed since latest matched report tx per oracle\n");
    body.push_str("# TYPE oracle_time_since_last_report_seconds gauge\n");

    // Keep output deterministic.
    let mut oracles: Vec<_> = snapshot.per_oracle.iter().collect();
    oracles.sort_by(|(a, _), (b, _)| a.cmp(b));

    for (oracle, metrics) in oracles {
        let present = if metrics.last_report_ts.is_some() { 1 } else { 0 };
        let last_ts = metrics.last_report_ts.unwrap_or(0);
        let age = metrics.time_since_last_report_secs.unwrap_or(0);

        body.push_str(&format!(
            "oracle_report_present{{oracle_address=\"{}\"}} {}\n",
            oracle, present
        ));
        body.push_str(&format!(
            "oracle_last_report_timestamp_seconds{{oracle_address=\"{}\"}} {}\n",
            oracle, last_ts
        ));
        body.push_str(&format!(
            "oracle_time_since_last_report_seconds{{oracle_address=\"{}\"}} {}\n",
            oracle, age
        ));
    }

    body
}

// Prometheus scrape endpoint.
async fn metrics_handler(State(state): State<SharedState>) -> impl IntoResponse {
    let snapshot = state.read().await.clone();
    render_metrics(&snapshot)
}

// Basic health endpoint.
// Returns 200 if last poll succeeded, otherwise 503.
async fn healthz_handler(State(state): State<SharedState>) -> impl IntoResponse {
    let snapshot = state.read().await.clone();
    if snapshot.last_check_ok {
        (StatusCode::OK, "ok")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "last check failed")
    }
}

// Start HTTP server for /metrics and /healthz.
pub async fn serve_http(state: SharedState) -> Result<()> {
    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/healthz", get(healthz_handler))
        .with_state(state);

    let listener = TcpListener::bind(METRICS_BIND_ADDR)
        .await
        .context("failed to bind metrics listener")?;

    axum::serve(listener, app)
        .await
        .context("metrics HTTP server failed")?;

    Ok(())
}

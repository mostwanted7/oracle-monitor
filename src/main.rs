mod app;
mod http_server;
mod utils;

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;

use crate::app::{build_initial_state, run_poller, SharedState};
use crate::http_server::serve_http;
use crate::utils::{load_config, METRICS_BIND_ADDR, POLL_INTERVAL_SECS};

#[tokio::main]
async fn main() -> Result<()> {
    // 1) Read startup config from CLI argument (`mainnet` or `hoodi`).
    let config = load_config()?;

    println!("Selected network: {}", config.network);
    println!("RPC URL: {}", config.rpc_url);
    println!("Target contract: {}", config.target_contract_address);
    println!("Poll interval seconds: {POLL_INTERVAL_SECS}");
    println!("Metrics endpoint: http://{METRICS_BIND_ADDR}/metrics");
    println!("Health endpoint:  http://{METRICS_BIND_ADDR}/healthz");

    // 2) Build shared app state.
    // Poller writes to it, HTTP endpoints read from it.
    let state: SharedState = Arc::new(RwLock::new(build_initial_state()));

    // 3) Build reusable HTTP client for JSON-RPC requests.
    let client = reqwest::Client::new();

    // 4) Spawn poller task in background.
    let poller_state = state.clone();
    let poller_client = client.clone();
    let poller_config = config.clone();
    tokio::spawn(async move {
        run_poller(poller_client, poller_config, poller_state).await;
    });

    // 5) Run HTTP server in foreground.
    serve_http(state).await?;

    Ok(())
}

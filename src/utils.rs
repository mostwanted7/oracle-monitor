use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};

// ---- App settings (easy place to tweak behavior) ----

// How far back to scan logs every poll.
pub const LOG_BLOCK_WINDOW: u64 = 100_000;

// Max number of newest unique tx hashes we inspect from logs.
// Increase this if you have many oracle members and miss reports.
pub const MAX_TX_TO_CHECK: usize = 300;

// Poll interval in seconds.
pub const POLL_INTERVAL_SECS: u64 = 900; // 15 minutes

// HTTP bind address for /metrics and /healthz.
pub const METRICS_BIND_ADDR: &str = "0.0.0.0:9104";

// ---- Runtime config ----

#[derive(Debug, Clone)]
pub struct Config {
    pub network: String,
    pub rpc_url: String,
    pub target_contract_address: String,
    pub members_source_address: String,
}

fn validate_eth_address(addr: &str) -> Result<()> {
    if addr.len() != 42 || !addr.starts_with("0x") {
        return Err(anyhow!("invalid ethereum address format: {addr}"));
    }

    let hex_part = &addr[2..];
    if !hex_part.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(anyhow!("invalid ethereum address hex: {addr}"));
    }

    Ok(())
}

// Parse CLI args and build config.
// Usage:
//   cargo run -- mainnet http://127.0.0.1:8545
//   cargo run -- hoodi   http://127.0.0.1:8545
//
// Optional env override:
//   ORACLE_MEMBERS_SOURCE_ADDRESS=0x...   (contract that has getMembers())
pub fn load_config() -> Result<Config> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 3 {
        return Err(anyhow!(
            "Usage: <binary> <network: mainnet|hoodi> <rpc_url>. Example: cargo run -- mainnet http://127.0.0.1:8545"
        ));
    }

    let network = args[1].as_str();
    let rpc_url = args[2].clone();

    let target_contract_address = match network {
        "mainnet" => "0xD624B08C83bAECF0807Dd2c6880C3154a5F0B288".to_string(),
        "hoodi" => "0x32EC59a78abaca3f91527aeB2008925D5AaC1eFC".to_string(),
        _ => {
            return Err(anyhow!(
                "Invalid network argument '{network}'. Use mainnet or hoodi"
            ));
        }
    };

    // Default contracts that expose getMembers() for each network.
    let default_members_source_address = match network {
        "mainnet" => "0x7FaDB6358950c5fAA66Cb5EB8eE5147De3df355a".to_string(),
        "hoodi" => "0x30308CD8844fb2DB3ec4D056F1d475a802DCA07c".to_string(),
        _ => unreachable!(),
    };

    let members_source_address = env::var("ORACLE_MEMBERS_SOURCE_ADDRESS")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or(default_members_source_address)
        .to_ascii_lowercase();

    validate_eth_address(&target_contract_address.to_ascii_lowercase())?;
    validate_eth_address(&members_source_address)?;

    Ok(Config {
        network: network.to_string(),
        rpc_url,
        target_contract_address,
        members_source_address,
    })
}

// ---- Small utility helpers ----

// Convert hex string into u64.
// Example: 0x10 -> 16
pub fn hex_to_u64(s: &str) -> Result<u64> {
    let trimmed = s.strip_prefix("0x").unwrap_or(s);
    u64::from_str_radix(trimmed, 16).context("invalid hex u64")
}

// Current unix time in seconds.
pub fn unix_now_secs() -> Result<u64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX_EPOCH")?;
    Ok(now.as_secs())
}

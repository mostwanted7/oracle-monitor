use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::utils::{hex_to_u64, unix_now_secs, Config, LOG_BLOCK_WINDOW, MAX_TX_TO_CHECK, POLL_INTERVAL_SECS};

// Metrics for one oracle address.
#[derive(Debug, Clone, Default)]
pub struct OracleMetrics {
    pub last_report_ts: Option<u64>,
    pub time_since_last_report_secs: Option<u64>,
}

// Shared monitor state.
// Poller task writes this, HTTP handlers read this.
#[derive(Debug, Clone, Default)]
pub struct MonitorState {
    pub per_oracle: HashMap<String, OracleMetrics>,
    pub last_check_ts: u64,
    pub last_check_ok: bool,
}

pub type SharedState = Arc<RwLock<MonitorState>>;

pub fn build_initial_state() -> MonitorState {
    MonitorState::default()
}

// Generic JSON-RPC helper.
async fn rpc_call(
    client: &reqwest::Client,
    rpc_url: &str,
    method: &str,
    params: Value,
) -> Result<Value> {
    let payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });

    let resp: Value = client
        .post(rpc_url)
        .json(&payload)
        .send()
        .await
        .context("rpc http request failed")?
        .json()
        .await
        .context("rpc response json decode failed")?;

    if let Some(err) = resp.get("error") {
        return Err(anyhow!("rpc error: {err}"));
    }

    resp.get("result")
        .cloned()
        .ok_or_else(|| anyhow!("rpc response missing result"))
}

// Encode plain text into hex-bytes string expected by web3_sha3.
// Example: "abc" -> "0x616263"
fn text_to_hex_bytes(s: &str) -> String {
    let mut out = String::from("0x");
    for b in s.as_bytes() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

// Ask node for method selector by hashing function signature.
// Example: "getMembers()" -> "0x..." (first 4 bytes / 8 hex chars + 0x)
async fn method_selector(client: &reqwest::Client, rpc_url: &str, signature: &str) -> Result<String> {
    let input_hex = text_to_hex_bytes(signature);
    let hash = rpc_call(client, rpc_url, "web3_sha3", json!([input_hex]))
        .await?
        .as_str()
        .context("web3_sha3 result was not a string")?
        .to_string();

    if hash.len() < 10 {
        return Err(anyhow!("web3_sha3 returned short hash for {signature}"));
    }

    Ok(hash[..10].to_string())
}

// Parse address from standard eth_call address return word (32-byte padded).
fn decode_address_word(result_hex: &str) -> Result<String> {
    let hex = result_hex
        .strip_prefix("0x")
        .ok_or_else(|| anyhow!("eth_call result missing 0x prefix"))?;

    if hex.len() < 64 {
        return Err(anyhow!("eth_call address result too short"));
    }

    // Address is the last 20 bytes (40 hex chars) of the first 32-byte word.
    let addr = format!("0x{}", &hex[24..64]).to_ascii_lowercase();
    Ok(addr)
}

fn read_abi_word_as_u64(hex_no_prefix: &str, byte_offset: usize) -> Result<u64> {
    let start = byte_offset * 2;
    let end = start + 64;

    if end > hex_no_prefix.len() {
        return Err(anyhow!("abi decode out of bounds"));
    }

    let word = &hex_no_prefix[start..end];
    u64::from_str_radix(word, 16).context("failed to parse abi word as u64")
}

// Decode only the first return item from getMembers(), which is `address[]`.
// getMembers() on HashConsensus returns a tuple where item 0 is members array.
fn decode_get_members_addresses(result_hex: &str) -> Result<Vec<String>> {
    let hex = result_hex
        .strip_prefix("0x")
        .ok_or_else(|| anyhow!("eth_call result missing 0x prefix"))?;

    // Head word #0 is byte offset to first dynamic item (members array).
    let members_offset = read_abi_word_as_u64(hex, 0)? as usize;

    // At members_offset: first word is array length.
    let members_len = read_abi_word_as_u64(hex, members_offset)? as usize;

    let mut out = Vec::with_capacity(members_len);
    for i in 0..members_len {
        let element_word_offset = members_offset + 32 + i * 32;
        let start = element_word_offset * 2;
        let end = start + 64;

        if end > hex.len() {
            return Err(anyhow!("abi decode members[] out of bounds"));
        }

        let word = &hex[start..end];
        let addr = format!("0x{}", &word[24..64]).to_ascii_lowercase();
        out.push(addr);
    }

    if out.is_empty() {
        return Err(anyhow!("getMembers returned empty members list"));
    }

    out.sort();
    out.dedup();
    Ok(out)
}

// Query on-chain oracle member list:
// AccountingOracle.getConsensusContract() -> HashConsensus.getMembers()
async fn fetch_oracle_allowlist(client: &reqwest::Client, config: &Config) -> Result<Vec<String>> {
    let get_consensus_selector = method_selector(client, &config.rpc_url, "getConsensusContract()")
        .await
        .context("failed to compute selector for getConsensusContract()")?;

    let consensus_raw = rpc_call(
        client,
        &config.rpc_url,
        "eth_call",
        json!([{
            "to": config.target_contract_address,
            "data": get_consensus_selector,
        }, "latest"]),
    )
    .await?
    .as_str()
    .context("getConsensusContract eth_call result was not a string")?
    .to_string();

    let hash_consensus_address = decode_address_word(&consensus_raw)
        .context("failed to decode consensus contract address")?;

    let get_members_selector = method_selector(client, &config.rpc_url, "getMembers()")
        .await
        .context("failed to compute selector for getMembers()")?;

    let members_raw = rpc_call(
        client,
        &config.rpc_url,
        "eth_call",
        json!([{
            "to": hash_consensus_address,
            "data": get_members_selector,
        }, "latest"]),
    )
    .await?
    .as_str()
    .context("getMembers eth_call result was not a string")?
    .to_string();

    decode_get_members_addresses(&members_raw).context("failed to decode getMembers() response")
}

// Returns newest report timestamp per allowed oracle address.
// Map key is lowercase oracle address.
async fn latest_report_timestamps(
    client: &reqwest::Client,
    config: &Config,
    allowlist: &[String],
) -> Result<HashMap<String, u64>> {
    // 1) Latest block number.
    let latest_hex = rpc_call(client, &config.rpc_url, "eth_blockNumber", json!([]))
        .await?
        .as_str()
        .context("eth_blockNumber result was not a string")?
        .to_string();
    let latest = hex_to_u64(&latest_hex)?;

    // 2) Define scan window.
    let from = latest.saturating_sub(LOG_BLOCK_WINDOW);
    let from_hex = format!("0x{from:x}");

    // 3) Get logs from target contract in that window.
    let logs_value = rpc_call(
        client,
        &config.rpc_url,
        "eth_getLogs",
        json!([{
            "fromBlock": from_hex,
            "toBlock": "latest",
            "address": config.target_contract_address,
        }]),
    )
    .await?;

    let logs = logs_value
        .as_array()
        .context("eth_getLogs result was not an array")?;

    let allowlist_set: HashSet<String> = allowlist.iter().cloned().collect();
    let target = config.target_contract_address.to_ascii_lowercase();

    // 4) Keep newest unique tx hashes.
    let mut seen = HashSet::new();
    let mut candidate_tx_hashes = Vec::new();

    for log in logs.iter().rev() {
        let Some(tx_hash) = log.get("transactionHash").and_then(Value::as_str) else {
            continue;
        };

        if seen.insert(tx_hash.to_string()) {
            candidate_tx_hashes.push(tx_hash.to_string());
            if candidate_tx_hashes.len() >= MAX_TX_TO_CHECK {
                break;
            }
        }
    }

    // 5) Resolve txs and keep first match per oracle.
    let mut per_oracle_latest = HashMap::new();
    let mut block_ts_cache = HashMap::new();

    for tx_hash in candidate_tx_hashes {
        if per_oracle_latest.len() >= allowlist_set.len() {
            break;
        }

        let tx = rpc_call(
            client,
            &config.rpc_url,
            "eth_getTransactionByHash",
            json!([tx_hash]),
        )
        .await?;

        if tx.is_null() {
            continue;
        }

        let from = tx
            .get("from")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let to = tx
            .get("to")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();

        if to != target || !allowlist_set.contains(&from) {
            continue;
        }

        if per_oracle_latest.contains_key(&from) {
            continue;
        }

        let Some(block_number_hex) = tx.get("blockNumber").and_then(Value::as_str) else {
            continue;
        };

        let timestamp = if let Some(ts) = block_ts_cache.get(block_number_hex) {
            *ts
        } else {
            let block = rpc_call(
                client,
                &config.rpc_url,
                "eth_getBlockByNumber",
                json!([block_number_hex, false]),
            )
            .await?;

            if block.is_null() {
                continue;
            }

            let timestamp_hex = block
                .get("timestamp")
                .and_then(Value::as_str)
                .context("block missing timestamp")?;
            let ts = hex_to_u64(timestamp_hex)?;
            block_ts_cache.insert(block_number_hex.to_string(), ts);
            ts
        };

        per_oracle_latest.insert(from, timestamp);
    }

    Ok(per_oracle_latest)
}

// One polling iteration.
async fn poll_once(client: &reqwest::Client, config: &Config, state: &SharedState) -> Result<()> {
    let now = unix_now_secs()?;

    match fetch_oracle_allowlist(client, config).await {
        Ok(allowlist) => {
            let latest_per_oracle = latest_report_timestamps(client, config, &allowlist).await?;

            let mut next_per_oracle = HashMap::new();
            for oracle in &allowlist {
                if let Some(last_report_ts) = latest_per_oracle.get(oracle) {
                    let age_secs = now.saturating_sub(*last_report_ts);
                    next_per_oracle.insert(
                        oracle.clone(),
                        OracleMetrics {
                            last_report_ts: Some(*last_report_ts),
                            time_since_last_report_secs: Some(age_secs),
                        },
                    );
                } else {
                    next_per_oracle.insert(oracle.clone(), OracleMetrics::default());
                }
            }

            let mut guard = state.write().await;
            guard.per_oracle = next_per_oracle;
            guard.last_check_ts = now;
            guard.last_check_ok = true;

            println!(
                "poll ok: allowlist_size={} found_reports={}",
                allowlist.len(),
                latest_per_oracle.len()
            );

            Ok(())
        }
        Err(err) => {
            // Keep previous values but mark check failure.
            let mut guard = state.write().await;
            guard.last_check_ts = now;
            guard.last_check_ok = false;

            Err(err)
        }
    }
}

// Background task: runs forever.
pub async fn run_poller(client: reqwest::Client, config: Config, state: SharedState) {
    let mut ticker = tokio::time::interval(Duration::from_secs(POLL_INTERVAL_SECS));

    loop {
        ticker.tick().await;

        if let Err(err) = poll_once(&client, &config, &state).await {
            eprintln!("poll failed: {err:#}");
        }
    }
}

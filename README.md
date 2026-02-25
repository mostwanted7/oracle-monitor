# Oracle Monitor

Small Rust service that monitors Lido oracle reports and exposes Prometheus metrics per each member of Oracle set.

## What it does

- Fetches oracle members on-chain via `getMembers()`
- Checks latest report tx per member
- Exposes metrics at `/metrics`
- Exposes health at `/healthz`

## Install and run

```bash
cargo build --release
./target/release/oracle-monitor <network> <rpc_url>
```

## Endpoints

- `http://0.0.0.0:9104/metrics`
- `http://0.0.0.0:9104/healthz`

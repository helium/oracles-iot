# HIP-0149 — Local Testing Plan with Production Data

## Overview

Two modes, depending on how much of the pipeline you want to exercise:

| Mode | What runs | What you need |
|---|---|---|
| **Basic** | Rewarder only — reads existing `gateway_dc_shares` from the snapshot | `iot_verifier_prod.dump` |
| **Full** | End-to-end: PacketLoader + Rewarder, local iot_config + price oracle | `iot_verifier_prod.dump` + `iot_config_prod.dump` + `iot_config_metadata.dump` + signing keypair |

All infrastructure runs in [`hip149/docker-compose.yml`](./docker-compose.yml).
Rust binaries (`iot_verifier`, `ingest`, `poc_entropy`) run on the host.

---

## Prerequisites

```bash
# Docker and docker compose
docker --version && docker compose version

# grpcurl for endpoint testing
brew install grpcurl

# Build the binaries
cargo build --release -p iot-verifier -p ingest -p poc-entropy
```

---

## Phase 1 — Take prod snapshots

### Always required

```bash
# iot_verifier DB (contains gateway_dc_shares, meta, gateway_shares)
pg_dump --no-owner --no-acl -Fc \
  "$PROD_IOT_VERIFIER_DATABASE_URL" \
  > iot_verifier_prod_$(date +%Y%m%d).dump
```

### Also required for full mode

```bash
# iot_config main DB (organizations, routes, gateways)
pg_dump --no-owner --no-acl -Fc \
  "$PROD_IOT_CONFIG_DATABASE_URL" \
  > iot_config_prod_$(date +%Y%m%d).dump

# iot_config metadata DB (Solana on-chain hotspot locations / gain)
pg_dump --no-owner --no-acl -Fc \
  "$PROD_IOT_CONFIG_METADATA_URL" \
  > iot_config_metadata_$(date +%Y%m%d).dump
```

---

## Phase 2 — Start infra

### Basic mode

```bash
DUMP_FILE=./iot_verifier_prod_$(date +%Y%m%d).dump \
  docker compose -f hip149/docker-compose.yml up -d

docker compose -f hip149/docker-compose.yml logs -f db-restore
# Wait for: "==> DB restore complete."
```

### Full mode

```bash
DUMP_FILE=./iot_verifier_prod_$(date +%Y%m%d).dump \
CONFIG_DB_DUMP=./iot_config_prod_$(date +%Y%m%d).dump \
METADATA_DB_DUMP=./iot_config_metadata_$(date +%Y%m%d).dump \
IOT_CONFIG_KEYPAIR=./iot_config_keypair.bin \
IOT_CONFIG_ADMIN=<admin_b58_pubkey> \
  docker compose -f hip149/docker-compose.yml --profile full up -d

# Watch all three restores
docker compose -f hip149/docker-compose.yml --profile full logs -f \
  db-restore config-db-restore metadata-db-restore
# Wait for all three "restore complete" lines.

# Then wait for price to emit its first file (~60s after startup):
docker compose -f hip149/docker-compose.yml --profile full logs -f price
# Watch for: "writing price report" or similar
```

Host ports after startup:

| Service | Port | Mode |
|---|---|---|
| iot_verifier postgres | `localhost:5433` | both |
| RustFS S3 API | `localhost:9100` | both |
| RustFS console | `localhost:9101` | both |
| iot_config config_db | `localhost:5434` | full only |
| iot_config metadata_db | `localhost:5435` | full only |
| iot_config gRPC | `localhost:9200` | full only |

---

## Phase 3 — Inspect snapshot state

```bash
psql "postgresql://postgres:postgres@localhost:5433/iot_verifier_hip149_test"
```

```sql
-- Epoch position
SELECT key, value FROM meta
WHERE key IN ('next_reward_epoch', 'disable_complete_data_checks_until', 'last_rewarded_end_time');

-- DC data in snapshot
SELECT
  COUNT(*)              AS dc_share_rows,
  SUM(num_dcs)          AS total_dcs,
  MIN(reward_timestamp) AS earliest,
  MAX(reward_timestamp) AS latest
FROM gateway_dc_shares;

-- Old PoC rows (should be frozen, never touched by new code)
SELECT COUNT(*) AS poc_rows FROM gateway_shares;

-- Hotspot count
SELECT COUNT(DISTINCT hotspot_key) AS hotspots_with_dc FROM gateway_dc_shares;
```

Record these numbers — you'll verify them after the run.

---

## Phase 4 — Configure and run iot_verifier

### Basic mode — use [`hip149/iot-verifier-settings.toml`](./iot-verifier-settings.toml)

Fill in the two TODOs (prod iot_config endpoint credentials and prod price bucket name), or
pass them as env vars:

```bash
AWS_ACCESS_KEY_ID=admin \
AWS_SECRET_ACCESS_KEY=admin \
VERIFY_IOT_CONFIG_CLIENT__SIGNING_KEYPAIR=/path/to/signing_keypair.bin \
VERIFY_IOT_CONFIG_CLIENT__CONFIG_PUBKEY=<prod_config_pubkey> \
./target/release/iot_verifier -c hip149/iot-verifier-settings.toml server
```

> The `iot_config_client` still points at the prod endpoint. This is read-only
> (gateway lookups only) so it's safe for testing.

### Full mode — use [`hip149/iot-verifier-settings-full.toml`](./iot-verifier-settings-full.toml)

```bash
AWS_ACCESS_KEY_ID=admin \
AWS_SECRET_ACCESS_KEY=admin \
VERIFY_IOT_CONFIG_CLIENT__SIGNING_KEYPAIR=/path/to/signing_keypair.bin \
VERIFY_IOT_CONFIG_CLIENT__CONFIG_PUBKEY=<local_iot_config_pubkey> \
./target/release/iot_verifier -c hip149/iot-verifier-settings-full.toml server
```

Key differences in `settings-full.toml`:
- `iot_config_client.url = "http://localhost:9200"` — local iot_config
- `price_tracker.bucket` points at `price-reports` bucket in RustFS on `localhost:9100`
- `file_store_clients.packet_input` points at `packet-ingest` bucket (seeded manually or
  by running the `ingest` binary against real gateway traffic)

### Force an immediate reward run (optional)

The rewarder fires on a 24h schedule. Override for testing:

```bash
VERIFY_REWARD_PERIOD=5m ./target/release/iot_verifier ...
```

Watch logs for:
- `"Rewarding for epoch N"` — triggered
- `"data transfer rewards complete"` — DC shares processed
- `"Successfully rewarded for epoch N"` — cycle complete
- No `ERROR` lines

---

## Phase 5 — Verify DB state after the run

```sql
-- 1. Epoch counter advanced
SELECT value FROM meta WHERE key = 'next_reward_epoch';
-- Expect: previous value + 1

-- 2. DC shares in the rewarded window cleaned up
SELECT COUNT(*), MAX(reward_timestamp) FROM gateway_dc_shares;
-- Expect: row count dropped

-- 3. gateway_shares (old PoC table) NOT touched
SELECT COUNT(*) AS poc_rows_after FROM gateway_shares;
-- Expect: same as Phase 3 count
```

---

## Phase 6 — Inspect reward output files

```bash
# List files in the rewards bucket
AWS_ACCESS_KEY_ID=admin AWS_SECRET_ACCESS_KEY=admin \
  aws --endpoint-url http://localhost:9100 s3 ls s3://iot-verifier-rewards/ --recursive

# Download for inspection
AWS_ACCESS_KEY_ID=admin AWS_SECRET_ACCESS_KEY=admin \
  aws --endpoint-url http://localhost:9100 s3 cp \
  s3://iot-verifier-rewards/<reward_file> /tmp/rewards.bin

AWS_ACCESS_KEY_ID=admin AWS_SECRET_ACCESS_KEY=admin \
  aws --endpoint-url http://localhost:9100 s3 cp \
  s3://iot-verifier-rewards/<manifest_file> /tmp/manifest.bin
```

---

## Phase 7 — Verify the reward math

Expected (E = epoch_emissions):

```
DC rewards allocated   ≤ E × 0.50   (capped at actual spend)
DC underflow           = (E × 0.50) − dc_rewards_allocated
Ops Fund reward        = E × 0.37 + dc_underflow
Oracle reward          = E × 0.07

Sum                    = E × 0.94   (routing 6% is on-chain, not oracle-emitted)

All GatewayReward entries:
  beacon_amount  == 0   (PoC retired)
  witness_amount == 0   (PoC retired)
  dc_transfer_amount > 0
```

Manifest spot-checks:
```
poc_bones_per_beacon_reward_share.value  == "0"
poc_bones_per_witness_reward_share.value == "0"
dc_bones_per_share.value                 != ""
```

Quick Python check once decoded to JSON:
```python
assert total_dc <= epoch_emissions * 0.50
dc_underflow = epoch_emissions * 0.50 - total_dc
assert abs(ops - (epoch_emissions * 0.37 + dc_underflow)) < 100
assert abs(oracle - epoch_emissions * 0.07) < 100
assert all(r["beacon_amount"] == 0 for r in gateway_rewards)
assert all(r["witness_amount"] == 0 for r in gateway_rewards)
```

---

## Phase 8 — Test ingest noops

```bash
INGEST_NETWORK=mainnet INGEST_LISTEN_ADDR=0.0.0.0:9080 \
  ./target/release/ingest server
```

```bash
# Beacon — expect { "id": "..." }, no file written
grpcurl -plaintext -d '{"pub_key": ""}' localhost:9080 \
  helium.poc_lora.poc_lora/submit_lora_beacon

# Witness — same
grpcurl -plaintext -d '{"pub_key": ""}' localhost:9080 \
  helium.poc_lora.poc_lora/submit_lora_witness

# Streaming session tests (automated, 9 tests)
cargo test -p ingest --test iot_ingest
```

---

## Phase 9 — Test entropy noop

```bash
ENTROPY_LISTEN=0.0.0.0:8082 ./target/release/poc_entropy server

grpcurl -plaintext -d '{}' localhost:8082 \
  helium.poc_entropy.poc_entropy/entropy
# Expect: { "data": "", "timestamp": <now>, "version": 0 }

# Load test
for i in $(seq 1 1000); do
  grpcurl -plaintext -d '{}' localhost:8082 helium.poc_entropy.poc_entropy/entropy > /dev/null
done && echo "1000 requests: OK"
```

---

## Phase 10 — Edge case: zero DC activity

```sql
TRUNCATE gateway_dc_shares;
-- disable_complete_data_checks_until already set by db-restore
```

Run the rewarder again. Expected:

```
dc_rewards_allocated  = 0
dc_underflow          = E × 0.50
ops_reward            = E × 0.87   (37% base + 50% underflow)
oracle_reward         = E × 0.07
No GatewayReward entries emitted
```

---

## Phase 11 — Full mode: verify price oracle

```bash
# Confirm price service is writing files (one per minute)
AWS_ACCESS_KEY_ID=admin AWS_SECRET_ACCESS_KEY=admin \
  aws --endpoint-url http://localhost:9100 s3 ls s3://price-reports/ --recursive
# Expect: price_report.* files timestamped ~1 minute apart
```

```bash
# Confirm iot_verifier read the price successfully
grep -i "price" <(RUST_LOG=iot_verifier=debug ./target/release/iot_verifier \
  -c hip149/iot-verifier-settings-full.toml server 2>&1) | head -5
# Look for: "current HNT price: ..." or no PriceNotAvailable errors
```

---

## Phase 12 — Prod cutover checklist

| Step | Action | Rollback |
|---|---|---|
| 1 | Deploy new `poc_entropy` | Redeploy old binary (no DB changes) |
| 2 | Deploy new `ingest` | Redeploy old binary (no DB changes) |
| 3 | Deploy new `iot_verifier` | Redeploy old binary — `gateway_shares` untouched |
| 4 | Monitor first epoch | `"Successfully rewarded"` in logs; ops ≈ 37% × emissions |
| 5 | After first clean epoch, truncate `gateway_shares` (optional) | N/A — cosmetic |

### Monitoring queries after cutover

```sql
SELECT COUNT(*), MAX(reward_timestamp) FROM gateway_dc_shares;
-- Row count drops to near zero after each reward run.

SELECT COUNT(*), MAX(reward_timestamp) FROM gateway_shares;
-- Frozen at pre-deploy values — never written to again.
```

---

## Teardown

```bash
# Basic mode
docker compose -f hip149/docker-compose.yml down -v

# Full mode
docker compose -f hip149/docker-compose.yml --profile full down -v
```

---

## Go / No-go criteria

| Check | Pass condition |
|---|---|
| Rewarder completes epoch | No `ERROR` in logs, epoch counter incremented |
| Reward math | Ops ≈ 37% × emissions + underflow (±100 bones) |
| No PoC amounts | All `beacon_amount` and `witness_amount` == 0 |
| Manifest PoC fields | Both `poc_bones_per_*` == `"0"` |
| DC shares cleaned | Rows in epoch window deleted after run |
| `gateway_shares` untouched | Row count unchanged from Phase 3 |
| Ingest beacon/witness | `{ "id": "..." }` returned, no file written |
| Entropy | Valid proto response, non-empty timestamp |
| Zero-DC edge case | Ops == 87% × emissions, no `GatewayReward` entries |
| Price oracle (full) | `price-reports` bucket has files; no `PriceNotAvailable` errors |
| iot_config (full) | `GatewayUpdater` logs gateway count on startup, no connection errors |

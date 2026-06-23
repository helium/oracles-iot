# HIP-0149 — PoC Retirement on IoT Network

## Context

[HIP-0149](https://github.com/helium/HIP/blob/main/0149-helium-utility-and-emissions-realignment.md) retires Proof of Coverage (PoC) on both the IoT and Mobile networks. This document describes all changes made to this repository to implement the IoT side of that decision.

---

## Reward Allocation Changes

The 30 % of epoch emissions previously split between beacon and witness rewards is absorbed by the **IoT Operations Fund**. DC transfer underflow (unused portion of the 50 % DC cap) that previously flowed back to PoC now also goes to the Operations Fund.

| Category | Before | After |
|---|---|---|
| Beacon (PoC) | 6 % | **0 %** — retired |
| Witness (PoC) | 24 % | **0 %** — retired |
| Data Transfer | 50 % (capped; underflow → PoC) | 50 % (capped; underflow → Ops) |
| Operations Fund | 7 % fixed | **37 % fixed + DC underflow** |
| Oracles | 7 % | 7 % |

> **Note on the 6 % gap:** The oracle-emitted buckets intentionally sum to 94 % of epoch emissions. The remaining 6 % ("Routing") is allocated at the on-chain sub-dao level and was never emitted by this oracle — this is unchanged from before HIP-0149.

---

## What Changed

### 1. Ingest — beacon and witness endpoints become noops

**`ingest/src/server_iot.rs`**

Gateways still connect and send PoC data. Rather than rejecting their connections (which would require gateway firmware updates), all three endpoints remain live but silently discard the data.

- `submit_lora_beacon` — validates public key and network, returns `LoraBeaconReportRespV1 { id }`, discards the report.
- `submit_lora_witness` — same pattern.
- `stream_requests` — session management (offer / init / timeout) is fully preserved. Beacon and witness messages arriving over the stream are validated (public key must match session, signature must be correct) then discarded. A bad signature or wrong public key still closes the stream, preserving the existing security boundary.

Removed from `GrpcServer`: `beacon_report_sink`, `witness_report_sink`, all S3 / file-upload setup. The struct now only carries `required_network`, `address`, and the two timeout durations.

---

### 2. PoC verification pipeline — removed entirely

The following source files were deleted from `iot_verifier/src/`:

| File | What it did |
|---|---|
| `loader.rs` | Read S3 beacon/witness ingest reports into the DB |
| `runner.rs` | Core PoC verification logic (RSSI, SNR, distance, denylist) |
| `poc.rs` | PoC report state machine |
| `entropy_loader.rs` | Loaded entropy files from S3 |
| `entropy.rs` | Entropy DB model |
| `hex_density.rs` | H3 hex density calculations for PoC scaling |
| `tx_scaler.rs` | Transmission-scale factor (density-based) |
| `last_beacon.rs` | Last-beacon timestamp tracking per hotspot |
| `last_beacon_reciprocity.rs` | HIP-106 reciprocity tracking |
| `last_witness.rs` | Last-witness timestamp tracking per hotspot |
| `witness_updater.rs` | Batch DB updates for witness state |
| `purger.rs` | Stale beacon/witness/entropy cleanup |
| `poc_report.rs` | `gateway_shares` DB model for PoC reward tracking |

The following tasks were removed from the `iot_verifier` daemon (`cli/server.rs`):
- `Loader` + ingest file source
- `EntropyLoader` + entropy gRPC server
- `Runner` + its three file sinks (invalid beacon, invalid witness, poc)
- `Purger` + its two file sinks
- `DensityScaler` / `TxScaler`
- `WitnessUpdater` + its gRPC server

**Kept:** `Rewarder`, `PacketLoader` (DC transfer), `GatewayUpdater` / `GatewayCache` (still required by `PacketLoader`), `PriceDaemon`, file-upload server, rewards / manifests file sinks.

---

### 3. Reward share logic

**`iot_verifier/src/reward_share.rs`**

- `OPERATIONS_REWARDS_PER_DAY_PERCENT` changed from `dec!(0.07)` to `dec!(0.37)`.
- `get_scheduled_ops_fund_tokens(epoch_emissions, dc_transfer_remainder)` now takes a second argument and returns `epoch_emissions * 0.37 + dc_transfer_remainder`.
- Removed: `BEACON_REWARDS_PER_DAY_PERCENT`, `WITNESS_REWARDS_PER_DAY_PERCENT`, `get_scheduled_poc_tokens`, `GatewayPocShare` struct (with `save` and `shares_from_poc`).
- `RewardShares` simplified to `dc_shares: Decimal` only.
- `GatewayShares::into_reward_shares` always sets `beacon_amount: 0, witness_amount: 0`.
- `GatewayShares::new` now returns `Self` directly (it was always infallible).
- `aggregate_reward_shares` no longer calls `aggregate_poc_shares`.
- `clear_rewarded_shares` only deletes from `gateway_dc_shares` (the `gateway_shares` table is no longer written to).

---

### 4. Rewarder

**`iot_verifier/src/rewarder.rs`**

- `reward_poc_and_dc` renamed to `reward_dc`. Returns `Decimal` (the DC underflow amount) instead of a composite struct.
- `reward_operational` gains a `dc_underflow: Decimal` parameter. Calls `get_scheduled_ops_fund_tokens(epoch_emissions, dc_underflow)` so the underflow is absorbed automatically.
- The dead `unallocated_operation_reward_amount` block was removed — `floor(x) - floor(x)` rounded to zero decimal places is always 0 and the `write_unallocated_reward` call for `UnallocatedRewardType::Operation` never triggered.
- `data_current_check` no longer checks the `gateway_shares` table (which is never written to); only `gateway_dc_shares` is checked.
- Reward manifest: `poc_bones_per_beacon_reward_share` and `poc_bones_per_witness_reward_share` are set to `Some("0")` to remain proto-compatible with downstream consumers.

---

### 5. Settings and telemetry cleanup

**`iot_verifier/src/settings.rs`**

Removed fields no longer referenced after the PoC removal:
- `base_stale_period`, `beacon_stale_period`, `witness_stale_period`, `entropy_stale_period`
- `max_witnesses_per_poc`, `beacon_interval`, `ingestor_rollup_time`
- `poc_loader_window_width`, `poc_loader_poll_time`, `entropy_interval`
- `beacon_max_retries`, `witness_max_retries`, `region_params_refresh_interval`
- `denylist` field
- `ingest_input` and `entropy_input` from `FileStoreClients`

`loader_window_max_lookback_age` is kept — it is still used by `PacketLoader`.

**`iot_verifier/src/telemetry.rs`**

Removed all PoC-specific metrics (`count_loader_beacons`, `count_loader_witnesses`, `increment_invalid_witnesses`, etc.). `LoaderMetricTracker` now only tracks `packets` and `non_rewardable_packets`.

---

### 6. `poc_entropy` — replaced with a noop stub

The old `poc_entropy` binary generated cryptographic entropy, wrote it to S3, and served it over a gRPC endpoint (`helium.poc_entropy.poc_entropy/entropy`). Since PoC beacons no longer need entropy, the generation and S3 pipeline have been removed. However, the gRPC endpoint is kept alive so that older gateway firmware that cannot be updated does not receive connection errors.

The new `poc_entropy` binary:
- Listens on the same address and implements the same `helium.poc_entropy.poc_entropy/entropy` RPC.
- Returns `EntropyReportV1 { data: [], timestamp: now, version: 0 }`.
- Has no external dependencies at startup (no S3, no entropy source, no blockchain).
- Retains the same `listen` config key and `ENTROPY_` environment variable prefix for backwards-compatible deployments.

---

## Tests

### Deleted
- `iot_verifier/tests/integrations/runner_tests.rs`
- `iot_verifier/tests/integrations/purger_tests.rs`
- `iot_verifier/tests/integrations/rewarder_poc_dc.rs`

### Added / rewritten
- **`rewarder_dc.rs`** — replaces `rewarder_poc_dc.rs`. Tests DC-only reward distribution: seeds two hotspots with DC shares (1 000 and 2 000 DCs), asserts correct per-hotspot amounts and that `allocated + dc_underflow == scheduled DC budget`.
- **`rewarder_operations.rs`** — updated to call `reward_operational` with the new 4-argument signature. Expected amount updated from 7 % (`6_232_876_712_328`) to 37 % (`32_945_205_479_452`). Added a second test (`test_operations_with_dc_underflow`) that seeds a full 50 % DC underflow and asserts the Ops Fund receives 87 % of epoch emissions.
- **`rewarder_iceberg.rs`** — `seed_minimal` updated to seed only `GatewayDCShare` rows (no more `GatewayPocShare`).
- **`ingest/tests/iot_ingest.rs`** — fully rewritten. `GrpcServer` no longer takes sink parameters. Tests that previously verified data arrived in sinks now verify only session-management behavior (stream closes on bad signatures, wrong pubkeys, timeouts). All 9 tests pass.

---

## Files Changed Summary

```
ingest/src/server_iot.rs              rewritten — noop beacon/witness, keep session validation
ingest/tests/iot_ingest.rs            rewritten — remove sink assertions, test session mechanics

iot_verifier/src/cli/server.rs        removed all PoC tasks from TaskManager
iot_verifier/src/lib.rs               removed 13 pub mod declarations
iot_verifier/src/reward_share.rs      new percentages, removed PoC types and functions
iot_verifier/src/rewarder.rs          reward_dc + reward_operational with underflow absorption
iot_verifier/src/settings.rs          removed PoC-specific settings fields
iot_verifier/src/telemetry.rs         removed PoC metrics

iot_verifier/tests/integrations/
  common/mod.rs                       removed PoC helpers and imports
  main.rs                             removed runner_tests, purger_tests; renamed rewarder_poc_dc
  rewarder_dc.rs                      new — DC-only reward test
  rewarder_iceberg.rs                 seed_minimal uses only GatewayDCShare
  rewarder_operations.rs              37% assertion + dc_underflow test

poc_entropy/                          replaced with noop gRPC stub (same endpoint, no generation)

Cargo.toml                            poc_entropy remains in workspace
.github/scripts/make_debian.sh        poc_entropy remains in build loop

Deleted:
  iot_verifier/src/{loader,runner,poc,entropy_loader,entropy,hex_density,
                    tx_scaler,last_beacon,last_beacon_reciprocity,last_witness,
                    witness_updater,purger,poc_report}.rs
  iot_verifier/tests/integrations/{runner_tests,purger_tests,rewarder_poc_dc}.rs
```

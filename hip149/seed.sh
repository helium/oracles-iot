#!/usr/bin/env bash
# ==============================================================================
# HIP-0149 seed script — generates test data from scratch.
# No prod dump files needed. Produces predictable, calculable reward amounts.
#
# What this script does:
#   1. Starts postgres, rustfs, config-db, and metadata-db via docker compose
#   2. Creates and migrates the iot_verifier_hip149_test database
#   3. Seeds meta + gateway_dc_shares with 3 test hotspots
#   4. Seeds metadata_db with sub_dao_epoch_infos for the test epoch
#   5. Prints exact expected reward values for verification
#
# Prerequisites:
#   - Docker + docker compose
#   - psql (brew install postgresql)
#   - cargo build --release -p iot-verifier (binary needed to RUN, not for seeding)
#
# Usage:
#   ./hip149/seed.sh
#
#   Optional overrides:
#     SUBDAO_KEY=<solana_base58_key>   Override the IoT SubDAO PDA key
#     EPOCH_OFFSET=2                   Days in the past for the test epoch (default: 2)
#     HNT_PRICE_OVERRIDE=100000000     HNT price in bones for expected math ($1.00 default)
# ==============================================================================
set -euo pipefail

# ─── Paths ───────────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# ─── Constants ───────────────────────────────────────────────────────────────

# The IoT SubDAO PDA (Solana base58).
# Derived from: find_program_address([b"sub_dao", IOT_MINT], helium_sub_daos::ID)
#   IOT_MINT              = iotEVVZLEywoTn1QdwNPddxPWszn3zFhEot3MfL9fns
#   helium_sub_daos::ID   = hdaoVTCqhfHHo75XdAMxBKdUqvq1i5bF23sisBqVgGR
# To verify: run iot_verifier and look for "Iot SubDao pubkey: ..." in the logs.
# Override with: SUBDAO_KEY=<key> ./hip149/seed.sh
SUBDAO_KEY="${SUBDAO_KEY:-39Lw1RH6zt8AJvKn3BTxmUDofzduCM2J3kSaGDZ8L7Sk}"

# Epoch: use N days in the past so the rewarder triggers immediately.
EPOCH_OFFSET="${EPOCH_OFFSET:-2}"

# HNT price used for expected-value math (must match PRICE__DEFAULT_PRICE in compose).
# 100_000_000 = $1.00 per HNT (8 decimal places).
HNT_PRICE="${HNT_PRICE_OVERRIDE:-100000000}"

# Test hotspot keys (valid helium PublicKeyBinary values from unit test constants).
HOTSPOT_A="112NqN2WWMwtK29PMzRby62fDydBJfsCLkCAf392stdok48ovNT6"
HOTSPOT_B="11uJHS2YaEWJqgqC7yza9uvSmpv5FWoMQXiP8WbxBGgNUmifUJf"
HOTSPOT_C="112E7TxoNHV46M6tiPA8N1MkeMeQxc9ztb4JQLXBVAAUfq1kJLoF"

# DC amounts per hotspot.
DC_A=1000
DC_B=2000
DC_C=4000
TOTAL_DC=$(( DC_A + DC_B + DC_C ))   # 7000

# Epoch emissions: 89_041_095_890_411 HNT bones per 24-hour epoch.
# (integration test constant EMISSIONS_POOL_IN_BONES_24_HOURS)
EMISSIONS=89041095890411

# ─── Derived epoch values ─────────────────────────────────────────────────────
NOW_SECS=$(date -u +%s)
EPOCH_DAY=$(( NOW_SECS / 86400 - EPOCH_OFFSET ))
EPOCH_START_SECS=$(( EPOCH_DAY * 86400 ))
EPOCH_END_SECS=$(( EPOCH_START_SECS + 86400 ))
DISABLE_CHECKS_UNTIL=$(( NOW_SECS + 604800 ))   # +7 days

# ─── Expected reward math ─────────────────────────────────────────────────────
# HNT price = $1 (HNT_PRICE=100_000_000 bones, 8 decimals)
# price_per_bone = 1.0 / 10^8 = 0.00000001 USD/bone
# DC → HNT bones: 1 DC costs $0.00001, so 1 DC = 0.00001/0.00000001 = 1000 bones
#
# dc_scheduled = floor(EMISSIONS * 0.50) = 44_520_547_945_205
# total_dc_bones = TOTAL_DC * 1000 = 7_000_000
# dc_underflow = dc_scheduled - total_dc_bones = 44_520_540_945_205
# ops = floor(EMISSIONS * 0.37) + dc_underflow = 32_945_205_479_452 + 44_520_540_945_205 = 77_465_746_424_657
# oracle = floor(EMISSIONS * 0.07) = 6_232_876_712_328
DC_BONES_PER_DC=1000
DC_A_BONES=$(( DC_A * DC_BONES_PER_DC ))
DC_B_BONES=$(( DC_B * DC_BONES_PER_DC ))
DC_C_BONES=$(( DC_C * DC_BONES_PER_DC ))
TOTAL_DC_BONES=$(( TOTAL_DC * DC_BONES_PER_DC ))

DC_SCHEDULED=$(( EMISSIONS * 50 / 100 ))
DC_UNDERFLOW=$(( DC_SCHEDULED - TOTAL_DC_BONES ))
OPS_BASE=$(( EMISSIONS * 37 / 100 ))
OPS_REWARD=$(( OPS_BASE + DC_UNDERFLOW ))
ORACLE_REWARD=$(( EMISSIONS * 7 / 100 ))
TOTAL_EMITTED=$(( TOTAL_DC_BONES + OPS_REWARD + ORACLE_REWARD ))

# ─── DB connection strings ────────────────────────────────────────────────────
VERIFIER_DB_URL="postgresql://postgres:postgres@localhost:5433/iot_verifier_hip149_test"
METADATA_DB_URL="postgresql://postgres:postgres@localhost:5435/metadata_db"

# ─── Helper ───────────────────────────────────────────────────────────────────
wait_for_pg() {
    local url="$1"
    local label="$2"
    echo "  Waiting for $label..."
    for _ in $(seq 1 30); do
        psql "$url" -c '\q' 2>/dev/null && return 0
        sleep 2
    done
    echo "ERROR: timed out waiting for $label" >&2
    exit 1
}

# ─── Banner ───────────────────────────────────────────────────────────────────
cat <<BANNER

══════════════════════════════════════════════════════════
  HIP-0149 seed script
══════════════════════════════════════════════════════════

  Epoch day:     $EPOCH_DAY
  Epoch window:  $(python3 -c "from datetime import datetime,timezone,timedelta; print(datetime(1970,1,1,tzinfo=timezone.utc)+timedelta(seconds=$EPOCH_START_SECS))") → $(python3 -c "from datetime import datetime,timezone; print(datetime.utcfromtimestamp($EPOCH_END_SECS).strftime('%Y-%m-%dT%H:%M:%SZ'))")
  Emissions:     $EMISSIONS HNT bones
  SubDAO key:    $SUBDAO_KEY
  HNT price:     \$1.00 (HNT_PRICE_OVERRIDE=$HNT_PRICE)

BANNER

# ─── Keypair generation ───────────────────────────────────────────────────────
# Generate a valid helium ed25519 keypair in binary format unless one was provided.
# Format: [tag=0x01, seed[32], pubkey[32]] = 65 bytes (KEYTYPE_ED25519, NETTYPE_MAIN).
# This avoids the need for helium-wallet keygen (which writes a wallet envelope,
# not the raw binary that helium_crypto::Keypair::try_from expects).
KEYPAIR_FILE="${IOT_CONFIG_KEYPAIR:-/tmp/test_keypair.bin}"

if [ ! -f "$KEYPAIR_FILE" ] || [ "$(wc -c < "$KEYPAIR_FILE")" -ne 65 ]; then
    echo "==> Generating helium ed25519 keypair at $KEYPAIR_FILE..."
    python3 - <<PYEOF
import os, hashlib

ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'

def b58encode_check(data):
    checksum = hashlib.sha256(hashlib.sha256(data).digest()).digest()[:4]
    payload = data + checksum
    num = int.from_bytes(payload, 'big')
    result = []
    while num > 0:
        result.append(ALPHABET[num % 58])
        num //= 58
    leading = sum(1 for b in payload if b == 0)
    return '1' * leading + ''.join(reversed(result))

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

seed = os.urandom(32)
private_key = Ed25519PrivateKey.from_private_bytes(seed)
pubkey_bytes = private_key.public_key().public_bytes_raw()

TAG = 0x01  # NETTYPE_MAIN | KEYTYPE_ED25519
keypair_bin = bytes([TAG]) + seed + pubkey_bytes
with open('$KEYPAIR_FILE', 'wb') as f:
    f.write(keypair_bin)

pubkey = b58encode_check(bytes([0x00, TAG]) + pubkey_bytes)
print(pubkey)
PYEOF
fi

# Derive the public key from the existing or just-generated keypair.
ADMIN_PUBKEY="${IOT_CONFIG_ADMIN:-}"
if [ -z "$ADMIN_PUBKEY" ]; then
    ADMIN_PUBKEY=$(python3 - <<PYEOF
import sys, hashlib

ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'

def b58encode_check(data):
    checksum = hashlib.sha256(hashlib.sha256(data).digest()).digest()[:4]
    payload = data + checksum
    num = int.from_bytes(payload, 'big')
    result = []
    while num > 0:
        result.append(ALPHABET[num % 58])
        num //= 58
    leading = sum(1 for b in payload if b == 0)
    return '1' * leading + ''.join(reversed(result))

data = open('$KEYPAIR_FILE', 'rb').read()
if len(data) != 65:
    print("ERROR: keypair file must be 65 bytes", file=sys.stderr)
    sys.exit(1)
tag = data[0]       # 0x01 for ed25519 mainnet
pubkey_bytes = data[33:]  # bytes 33..65

print(b58encode_check(bytes([0x00, tag]) + pubkey_bytes))
PYEOF
    )
fi

export IOT_CONFIG_KEYPAIR="$KEYPAIR_FILE"
export IOT_CONFIG_ADMIN="$ADMIN_PUBKEY"
echo "  Keypair: $KEYPAIR_FILE"
echo "  Admin:   $ADMIN_PUBKEY"

# Save to /tmp/hip149_env for use in subsequent steps.
cat > /tmp/hip149_env <<ENV
export IOT_CONFIG_KEYPAIR="$KEYPAIR_FILE"
export IOT_CONFIG_ADMIN="$ADMIN_PUBKEY"
export ADMIN_PUBKEY="$ADMIN_PUBKEY"
ENV

# Patch iot-verifier-settings-full.toml so config_pubkey matches the generated keypair.
SETTINGS_TOML="$SCRIPT_DIR/iot-verifier-settings-full.toml"
if [ -f "$SETTINGS_TOML" ]; then
    sed -i.bak "s|^config_pubkey = \".*\"|config_pubkey = \"$ADMIN_PUBKEY\"|" "$SETTINGS_TOML"
    sed -i.bak "s|^signing_keypair = \".*\"|signing_keypair = \"$KEYPAIR_FILE\"|" "$SETTINGS_TOML"
    rm -f "${SETTINGS_TOML}.bak"
    echo "  Updated $SETTINGS_TOML with current keypair"
fi

# ─── Step 1: Start infra ──────────────────────────────────────────────────────
echo "==> [1/4] Starting postgres, rustfs, config-db, metadata-db..."

docker compose -f hip149/docker-compose.yml --profile full \
    up -d postgres rustfs config-db metadata-db

wait_for_pg "postgresql://postgres:postgres@localhost:5433/postgres" "verifier postgres (5433)"
wait_for_pg "postgresql://postgres:postgres@localhost:5435/metadata_db" "metadata-db (5435)"

# ─── Step 2: Create and migrate iot_verifier DB ───────────────────────────────
echo ""
echo "==> [2/4] Creating and migrating iot_verifier_hip149_test..."

psql "postgresql://postgres:postgres@localhost:5433/postgres" \
    -c "DROP DATABASE IF EXISTS iot_verifier_hip149_test;" \
    -c "CREATE DATABASE iot_verifier_hip149_test;" \
    -q

# Apply migrations in numeric order (1, 2, 3, ..., 10, 11, ...)
for n in 1 2 3 4 5 6 7 9 10 11 12 13 14 15; do
    for mig in "$REPO_ROOT/iot_verifier/migrations/${n}_"*.sql; do
        [ -f "$mig" ] || continue
        psql "$VERIFIER_DB_URL" -f "$mig" -q
    done
done

# Populate _sqlx_migrations so the binary doesn't try to re-run them.
# SQLx checksum = SHA-384 of the raw file bytes; execution_time in nanoseconds.
python3 - <<PYEOF
import hashlib, os, subprocess, sys

migrations_dir = '$REPO_ROOT/iot_verifier/migrations'
files = sorted(f for f in os.listdir(migrations_dir) if f.endswith('.sql'))

values = []
for f in files:
    base = f.rsplit('.sql', 1)[0]
    idx = base.index('_')
    version = int(base[:idx])
    description = base[idx+1:].replace('_', ' ')
    with open(os.path.join(migrations_dir, f), 'rb') as fh:
        checksum = hashlib.sha384(fh.read()).digest().hex()
    values.append(f"({version}, '{description}', NOW(), TRUE, '\\\\x{checksum}'::bytea, 0)")

sql = (
    "CREATE TABLE IF NOT EXISTS _sqlx_migrations ("
    "  version BIGINT NOT NULL PRIMARY KEY,"
    "  description TEXT NOT NULL,"
    "  installed_on TIMESTAMPTZ NOT NULL DEFAULT NOW(),"
    "  success BOOL NOT NULL,"
    "  checksum BYTEA NOT NULL,"
    "  execution_time BIGINT NOT NULL"
    "); INSERT INTO _sqlx_migrations (version, description, installed_on, success, checksum, execution_time)"
    " VALUES " + ",".join(values) +
    " ON CONFLICT (version) DO NOTHING;"
)
r = subprocess.run(['psql', '$VERIFIER_DB_URL', '-c', sql], capture_output=True, text=True)
if r.returncode != 0:
    print(r.stderr, file=sys.stderr)
    sys.exit(1)
PYEOF
echo "  Migrations applied."

# ─── Step 3: Seed iot_verifier DB ─────────────────────────────────────────────
echo ""
echo "==> [3/4] Seeding iot_verifier DB..."

psql "$VERIFIER_DB_URL" -q <<SQL
-- Epoch state: rewarder processes epoch $EPOCH_DAY on next run.
-- disable_complete_data_checks_until is set far in the future so the
-- rewarder skips the "is fresh data available?" gate (not needed for seeded data).
INSERT INTO meta (key, value) VALUES
    ('next_reward_epoch',                   '$EPOCH_DAY'),
    ('last_rewarded_end_time',              '$EPOCH_START_SECS'),
    ('disable_complete_data_checks_until',  '$DISABLE_CHECKS_UNTIL')
ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value;

-- DC shares: 3 hotspots, timestamped inside the epoch window
-- (reward_timestamp must be > epoch_start AND <= epoch_end).
INSERT INTO gateway_dc_shares (hotspot_key, reward_timestamp, num_dcs, id) VALUES
    ('$HOTSPOT_A', to_timestamp($(( EPOCH_START_SECS + 3600 ))),  $DC_A, '\x01'::bytea),
    ('$HOTSPOT_B', to_timestamp($(( EPOCH_START_SECS + 7200 ))),  $DC_B, '\x02'::bytea),
    ('$HOTSPOT_C', to_timestamp($(( EPOCH_START_SECS + 10800 ))), $DC_C, '\x03'::bytea)
ON CONFLICT (id) DO UPDATE SET num_dcs = EXCLUDED.num_dcs;
SQL

echo "  meta: next_reward_epoch=$EPOCH_DAY, disable_checks_until=$DISABLE_CHECKS_UNTIL"
echo "  gateway_dc_shares: 3 rows (${DC_A} + ${DC_B} + ${DC_C} = ${TOTAL_DC} DC)"

# ─── Step 4: Seed metadata_db (sub_dao_epoch_infos) ──────────────────────────
echo ""
echo "==> [4/4] Seeding metadata_db (sub_dao_epoch_infos)..."

psql "$METADATA_DB_URL" -q <<SQL
-- Create the table if it doesn't exist.
-- This table is normally populated by an on-chain scraper, not by iot_config migrations.
CREATE TABLE IF NOT EXISTS sub_dao_epoch_infos (
    address                   TEXT        NOT NULL,
    sub_dao                   TEXT        NOT NULL,
    epoch                     BIGINT      NOT NULL,
    hnt_rewards_issued        BIGINT      NOT NULL DEFAULT 0,
    delegation_rewards_issued BIGINT      NOT NULL DEFAULT 0,
    rewards_issued_at         BIGINT      NOT NULL,
    PRIMARY KEY (sub_dao, epoch)
);

-- epoch_emissions = hnt_rewards_issued + delegation_rewards_issued.
-- Both must be > 0 (enforced by iot_config's FromRow deserialization).
-- We split: hnt = EMISSIONS - 1_000_000_000, delegation = 1_000_000_000.
INSERT INTO sub_dao_epoch_infos
    (address, sub_dao, epoch, hnt_rewards_issued, delegation_rewards_issued, rewards_issued_at)
VALUES (
    'seed-epoch-addr',
    '$SUBDAO_KEY',
    $EPOCH_DAY,
    $(( EMISSIONS - 1000000000 )),
    1000000000,
    $EPOCH_END_SECS
)
ON CONFLICT (sub_dao, epoch) DO UPDATE SET
    hnt_rewards_issued        = EXCLUDED.hnt_rewards_issued,
    delegation_rewards_issued = EXCLUDED.delegation_rewards_issued,
    rewards_issued_at         = EXCLUDED.rewards_issued_at;
SQL

echo "  sub_dao_epoch_infos: epoch=$EPOCH_DAY, sub_dao=$SUBDAO_KEY"
echo "  hnt_rewards_issued=$(( EMISSIONS - 1000000000 )), delegation=1000000000, total=$EMISSIONS"

# ─── Wait for RustFS buckets ──────────────────────────────────────────────────
echo ""
echo "==> Waiting for RustFS (localhost:9100)..."
for _ in $(seq 1 30); do
    curl -sf http://localhost:9100/health >/dev/null 2>&1 && break
    sleep 2
done
echo "  RustFS ready."

# ─── Summary ──────────────────────────────────────────────────────────────────
cat <<SUMMARY

══════════════════════════════════════════════════════════
  Seed complete! Expected reward output:
══════════════════════════════════════════════════════════

  GatewayReward entries (3 hotspots):
    HOTSPOT_A  dc_transfer_amount = $DC_A_BONES   (beacon=0, witness=0)
    HOTSPOT_B  dc_transfer_amount = $DC_B_BONES   (beacon=0, witness=0)
    HOTSPOT_C  dc_transfer_amount = $DC_C_BONES   (beacon=0, witness=0)

  OperationalReward:
    ops_amount = $OPS_REWARD
      = floor(EMISSIONS × 0.37) + dc_underflow
      = $OPS_BASE + $DC_UNDERFLOW

  UnallocatedReward (oracle):
    oracle_amount = $ORACLE_REWARD
      = floor(EMISSIONS × 0.07)

  Total emitted = $TOTAL_EMITTED  (94% of $EMISSIONS emissions)

  Verification checks:
    All GatewayReward.beacon_amount  == 0
    All GatewayReward.witness_amount == 0
    Sum(dc_transfer_amount)          == $TOTAL_DC_BONES
    ops + oracle + dc                == $TOTAL_EMITTED

══════════════════════════════════════════════════════════
  Next steps:
══════════════════════════════════════════════════════════

  Keypair:   $KEYPAIR_FILE
  Admin key: $ADMIN_PUBKEY
  (source /tmp/hip149_env to re-use these in your shell)

  1. Start iot-config and price (--profile full):

       source /tmp/hip149_env
       HNT_PRICE_OVERRIDE=$HNT_PRICE \\
         docker compose -f hip149/docker-compose.yml --profile full \\
         up -d iot-config price

     Wait ~60s for the price service to emit its first price_report file.

  2. Run iot_verifier:

       source /tmp/hip149_env
       AWS_ACCESS_KEY_ID=admin \\
       AWS_SECRET_ACCESS_KEY=admin \\
       VERIFY_IOT_CONFIG_CLIENT__SIGNING_KEYPAIR=\$IOT_CONFIG_KEYPAIR \\
       VERIFY_IOT_CONFIG_CLIENT__CONFIG_PUBKEY=\$ADMIN_PUBKEY \\
         ./target/release/iot_verifier \\
         -c hip149/iot-verifier-settings-full.toml server

     Watch for:
       "Iot SubDao pubkey: <key>"      ← verify matches SUBDAO_KEY=$SUBDAO_KEY
       "Rewarding for epoch $EPOCH_DAY"
       "Successfully rewarded for epoch $EPOCH_DAY"

  3. If the SubDAO pubkey in the logs does NOT match "$SUBDAO_KEY":
     Re-run with the correct key:
       SUBDAO_KEY=<logged_key> ./hip149/seed.sh

  4. Inspect rewards:
       AWS_ACCESS_KEY_ID=admin AWS_SECRET_ACCESS_KEY=admin \\
         aws --endpoint-url http://localhost:9100 s3 ls s3://iot-verifier-rewards/ --recursive

  5. Teardown:
       docker compose -f hip149/docker-compose.yml --profile full down -v

SUMMARY

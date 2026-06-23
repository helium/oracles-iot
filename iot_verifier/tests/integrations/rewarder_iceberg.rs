//! End-to-end integration tests for the rewarder's iceberg live-write path.
//!
//! These tests drive `Rewarder::reward(epoch_day)` directly so the production
//! code performs the iceberg writes (lines 214-228 of `rewarder.rs`). The test
//! only sets up the inputs (db seed, sinks with drainers, iceberg writers
//! sourced from the test harness) and verifies the iceberg state via Trino.

use crate::common::iceberg::setup_iceberg;
use crate::common::{
    self, rewards_info_24_hours, spawn_file_sink_drainer, MockSubDaoEpochRewardInfoResolver,
    TestPriceProvider,
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use helium_iceberg::IcebergTestHarness;
use helium_proto::{services::poc_lora::IotRewardShare, RewardManifest};
use iot_verifier::{
    iceberg::{gateway_reward, operational_reward, unallocated_reward, RewardWriters, NAMESPACE},
    reward_share::GatewayDCShare,
    rewarder::Rewarder,
};
use prost::Message;
use rust_decimal_macros::dec;
use sqlx::{PgPool, Postgres, Transaction};
use std::time::Duration;
use trino_rust_client::Trino;

const HOTSPOT_1: &str = "112NqN2WWMwtK29PMzRby62fDydBJfsCLkCAf392stdok48ovNT6";

#[sqlx::test]
async fn reward_writes_gateway_rows_to_iceberg(pool: PgPool) -> anyhow::Result<()> {
    let harness = setup_iceberg().await?;
    let writers = writers(&harness).await?;

    let reward_info = rewards_info_24_hours();
    let mut txn = pool.clone().begin().await?;
    seed_minimal(reward_info.epoch_period.start, &mut txn).await?;
    txn.commit().await?;

    let (mut rewarder, rewards_drainer, manifests_drainer) =
        build_rewarder(pool.clone(), writers).await?;

    rewarder.reward(reward_info.epoch_day).await?;

    rewards_drainer.abort();
    manifests_drainer.abort();

    let gw: Vec<GatewayRewardRow> = get_all_or_empty(
        harness.trino(),
        format!("SELECT * FROM {}.{}", NAMESPACE, gateway_reward::TABLE_NAME),
    )
    .await?;
    assert!(!gw.is_empty(), "gateway iceberg table should not be empty");
    assert!(
        gw.iter()
            .any(|r| r.hotspot_key == HOTSPOT_1 && r.dc_transfer_amount > 0),
        "expected a row for HOTSPOT_1 with non-zero dc_transfer_amount, got {gw:?}"
    );

    Ok(())
}

#[sqlx::test]
async fn reward_writes_operational_to_iceberg(pool: PgPool) -> anyhow::Result<()> {
    let harness = setup_iceberg().await?;
    let writers = writers(&harness).await?;

    // No POC/DC seed: operational rewards are a fixed cut of `epoch_emissions`
    // and always produce a row.
    let reward_info = rewards_info_24_hours();
    let (mut rewarder, rewards_drainer, manifests_drainer) =
        build_rewarder(pool.clone(), writers).await?;

    rewarder.reward(reward_info.epoch_day).await?;

    rewards_drainer.abort();
    manifests_drainer.abort();

    let ops: Vec<OperationalRewardRow> = get_all_or_empty(
        harness.trino(),
        format!(
            "SELECT * FROM {}.{}",
            NAMESPACE,
            operational_reward::TABLE_NAME
        ),
    )
    .await?;
    assert_eq!(ops.len(), 1, "exactly one operational row per epoch");
    assert!(ops[0].amount > 0, "operational amount must be positive");

    Ok(())
}

#[sqlx::test]
async fn reward_writes_unallocated_oracle_to_iceberg(pool: PgPool) -> anyhow::Result<()> {
    let harness = setup_iceberg().await?;
    let writers = writers(&harness).await?;

    let reward_info = rewards_info_24_hours();
    let (mut rewarder, rewards_drainer, manifests_drainer) =
        build_rewarder(pool.clone(), writers).await?;

    rewarder.reward(reward_info.epoch_day).await?;

    rewards_drainer.abort();
    manifests_drainer.abort();

    let rows: Vec<UnallocatedRewardRow> = get_all_or_empty(
        harness.trino(),
        format!(
            "SELECT * FROM {}.{}",
            NAMESPACE,
            unallocated_reward::TABLE_NAME
        ),
    )
    .await?;
    assert!(
        rows.iter()
            .any(|r| r.reward_type == "Oracle" && r.amount > 0),
        "expected an Oracle unallocated row with non-zero amount, got {rows:?}"
    );

    Ok(())
}

#[sqlx::test]
async fn full_epoch_writes_all_three_tables(pool: PgPool) -> anyhow::Result<()> {
    let harness = setup_iceberg().await?;
    let writers = writers(&harness).await?;

    let reward_info = rewards_info_24_hours();
    let mut txn = pool.clone().begin().await?;
    seed_minimal(reward_info.epoch_period.start, &mut txn).await?;
    txn.commit().await?;

    let (mut rewarder, rewards_drainer, manifests_drainer) =
        build_rewarder(pool.clone(), writers).await?;

    rewarder.reward(reward_info.epoch_day).await?;

    rewards_drainer.abort();
    manifests_drainer.abort();

    let gw: Vec<GatewayRewardRow> = get_all_or_empty(
        harness.trino(),
        format!("SELECT * FROM {}.{}", NAMESPACE, gateway_reward::TABLE_NAME),
    )
    .await?;
    let ops: Vec<OperationalRewardRow> = get_all_or_empty(
        harness.trino(),
        format!(
            "SELECT * FROM {}.{}",
            NAMESPACE,
            operational_reward::TABLE_NAME
        ),
    )
    .await?;
    let unalloc: Vec<UnallocatedRewardRow> = get_all_or_empty(
        harness.trino(),
        format!(
            "SELECT * FROM {}.{}",
            NAMESPACE,
            unallocated_reward::TABLE_NAME
        ),
    )
    .await?;

    assert!(!gw.is_empty(), "gateway table should have rows");
    assert_eq!(ops.len(), 1, "one operational row per epoch");
    assert!(!unalloc.is_empty(), "unallocated table should have rows");

    Ok(())
}

#[derive(Debug, Clone, Trino, serde::Serialize, serde::Deserialize, PartialEq)]
struct GatewayRewardRow {
    hotspot_key: String,
    beacon_amount: i64,
    witness_amount: i64,
    dc_transfer_amount: i64,
    start_period: chrono::DateTime<chrono::FixedOffset>,
    end_period: chrono::DateTime<chrono::FixedOffset>,
}

#[derive(Debug, Clone, Trino, serde::Serialize, serde::Deserialize, PartialEq)]
struct OperationalRewardRow {
    amount: i64,
    start_period: chrono::DateTime<chrono::FixedOffset>,
    end_period: chrono::DateTime<chrono::FixedOffset>,
}

#[derive(Debug, Clone, Trino, serde::Serialize, serde::Deserialize, PartialEq)]
struct UnallocatedRewardRow {
    reward_type: String,
    amount: i64,
    start_period: chrono::DateTime<chrono::FixedOffset>,
    end_period: chrono::DateTime<chrono::FixedOffset>,
}

async fn get_all_or_empty<T>(
    trino: &trino_rust_client::Client,
    sql: String,
) -> anyhow::Result<Vec<T>>
where
    T: trino_rust_client::Trino + serde::Serialize + for<'de> serde::Deserialize<'de> + 'static,
{
    match trino.get_all::<T>(sql).await {
        Ok(rows) => Ok(rows.into_vec()),
        Err(trino_rust_client::error::Error::EmptyData) => Ok(vec![]),
        Err(e) => Err(e.into()),
    }
}

/// Builds a `RewardWriters` whose individual writers come from the test
/// `IcebergTestHarness`. These are the same writer types the production
/// rewarder constructs from `iceberg_settings`.
async fn writers(harness: &IcebergTestHarness) -> anyhow::Result<RewardWriters> {
    Ok(RewardWriters {
        gateway: harness.get_table_writer(gateway_reward::TABLE_NAME).await?,
        operational: harness
            .get_table_writer(operational_reward::TABLE_NAME)
            .await?,
        unallocated: harness
            .get_table_writer(unallocated_reward::TABLE_NAME)
            .await?,
    })
}

/// Constructs a `Rewarder` wired up for end-to-end testing: real DB pool,
/// real iceberg writers from the harness, mock sub-dao + price provider,
/// and proto sinks that are drained in the background. Returns the rewarder
/// alongside drainer task handles so the caller can drop them after
/// `reward()` returns.
async fn build_rewarder(
    pool: PgPool,
    reward_writers: RewardWriters,
) -> anyhow::Result<(
    Rewarder<MockSubDaoEpochRewardInfoResolver, TestPriceProvider>,
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<()>,
)> {
    let (rewards_sink, iot_rewards_rx) = common::create_file_sink::<IotRewardShare>();
    let (manifests_sink, manifests_rx) = common::create_file_sink::<RewardManifest>();

    let rewards_drainer = spawn_file_sink_drainer(iot_rewards_rx);
    let manifests_drainer = spawn_file_sink_drainer(manifests_rx);

    let reward_info = rewards_info_24_hours();
    // Matches `default_price_info()` — `Rewarder::reward` wraps this in
    // `PriceInfo::new(price, Token::Hnt.decimals())`.
    let price_provider = TestPriceProvider::new(1);
    let sub_dao_client = MockSubDaoEpochRewardInfoResolver::new(reward_info);

    let rewarder = Rewarder::new(
        pool,
        rewards_sink,
        manifests_sink,
        Duration::from_secs(24 * 60 * 60),
        Duration::from_secs(0),
        price_provider,
        sub_dao_client,
        Some(reward_writers),
    )?;

    Ok((rewarder, rewards_drainer, manifests_drainer))
}

/// Minimal DC seed: one DC share for HOTSPOT_1. Enough to produce non-zero
/// `gateway`, `operational`, and `unallocated` rows from the rewarder.
async fn seed_minimal(
    ts: DateTime<Utc>,
    txn: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<()> {
    GatewayDCShare {
        hotspot_key: HOTSPOT_1.parse().unwrap(),
        reward_timestamp: ts + ChronoDuration::hours(1),
        num_dcs: dec!(1000),
        id: "dc_id_1".to_string().encode_to_vec(),
    }
    .save(txn)
    .await?;
    Ok(())
}

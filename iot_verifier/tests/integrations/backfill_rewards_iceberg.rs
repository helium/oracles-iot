use chrono::{Duration, Utc};
use file_store::aws_local::AwsLocal;
use file_store_oracles::FileType;
use helium_crypto::PublicKeyBinary;
use helium_iceberg::{BatchedWriter, BatchedWriterConfig, IcebergTable, IcebergTestHarness};
use helium_proto::services::poc_lora::{IotRewardShare, UnallocatedRewardType};
use iot_verifier::iceberg::{
    gateway_reward, operational_reward, unallocated_reward, IcebergIotGatewayReward,
    IcebergIotOperationalReward, IcebergIotUnallocatedReward,
};
use iot_verifier::{
    backfill::{
        rewards::{IotRewardsBackfiller, IotRewardsFanoutWriter},
        BackfillOptions,
    },
    iceberg::NAMESPACE,
};
use sqlx::PgPool;
use tempfile::TempDir;
use trino_rust_client::Trino;

/// `Trino::get_all` returns `EmptyData` rather than an empty vec when a query
/// produces no rows. Smooth that over for assertion ergonomics.
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

use crate::common::iceberg::{
    gateway_reward_share, operational_reward_share, setup_iceberg, test_backfill_options,
    unallocated_reward_share,
};

const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

// Iceberg/Trino round-trips `u64` (Long) → `i64`. Local types here mirror the
// production row shapes but use signed integers so trino-rust-client can
// deserialize natively without a u64 codec.
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

#[sqlx::test]
async fn backfill_writes_all_three_reward_variants(pool: PgPool) -> anyhow::Result<()> {
    let harness = setup_iceberg().await?;
    let (writer, tasks, _spools) = fanout_writer(&harness).await?;

    let awsl = AwsLocal::new().await;
    awsl.create_bucket().await?;

    let pubkey: PublicKeyBinary = "112NqN2WWMwtK29PMzRby62fDydBJfsCLkCAf392stdok48ovNT6".parse()?;
    let base = Utc::now() - Duration::hours(1);
    let start_time = base - Duration::minutes(1);
    let end_time = base + Duration::days(1);

    let start_period = base.timestamp() as u64;
    let end_period = (base + Duration::hours(1)).timestamp() as u64;

    put_share_at(
        &awsl,
        gateway_reward_share(
            pubkey.as_ref().to_vec(),
            10,
            20,
            30,
            start_period,
            end_period,
        ),
        base,
    )
    .await?;
    put_share_at(
        &awsl,
        operational_reward_share(7_000, start_period, end_period),
        base + Duration::seconds(1),
    )
    .await?;
    put_share_at(
        &awsl,
        unallocated_reward_share(UnallocatedRewardType::Poc, 5_000, start_period, end_period),
        base + Duration::seconds(2),
    )
    .await?;

    let opts = test_backfill_options("rewards-backfill-all-variants", start_time, end_time);
    run_backfill(pool, awsl.bucket_client(), writer.clone(), opts).await?;
    writer.flush_all().await?;
    tasks.abort_all();

    let gateways: Vec<GatewayRewardRow> = get_all_or_empty(
        harness.trino(),
        format!("SELECT * FROM {}.{}", NAMESPACE, gateway_reward::TABLE_NAME),
    )
    .await?;
    assert_eq!(gateways.len(), 1, "expected 1 gateway reward");
    assert_eq!(gateways[0].hotspot_key, pubkey.to_string());
    assert_eq!(gateways[0].beacon_amount, 10);
    assert_eq!(gateways[0].witness_amount, 20);
    assert_eq!(gateways[0].dc_transfer_amount, 30);

    let ops: Vec<OperationalRewardRow> = get_all_or_empty(
        harness.trino(),
        format!(
            "SELECT * FROM {}.{}",
            NAMESPACE,
            operational_reward::TABLE_NAME
        ),
    )
    .await?;
    assert_eq!(ops.len(), 1, "expected 1 operational reward");
    assert_eq!(ops[0].amount, 7_000);

    let unalloc: Vec<UnallocatedRewardRow> = get_all_or_empty(
        harness.trino(),
        format!(
            "SELECT * FROM {}.{}",
            NAMESPACE,
            unallocated_reward::TABLE_NAME
        ),
    )
    .await?;
    assert_eq!(unalloc.len(), 1, "expected 1 unallocated reward");
    assert_eq!(unalloc[0].reward_type, "Poc");
    assert_eq!(unalloc[0].amount, 5_000);

    awsl.cleanup().await?;
    Ok(())
}

#[sqlx::test]
async fn backfill_skips_files_after_stop_after(pool: PgPool) -> anyhow::Result<()> {
    let harness = setup_iceberg().await?;
    let (writer, tasks, _spools) = fanout_writer(&harness).await?;

    let awsl = AwsLocal::new().await;
    awsl.create_bucket().await?;

    let pubkey: PublicKeyBinary = "112NqN2WWMwtK29PMzRby62fDydBJfsCLkCAf392stdok48ovNT6".parse()?;
    let base = Utc::now() - Duration::hours(2);
    let start_time = base - Duration::minutes(1);
    let early_time = base;
    let stop_time = base + Duration::minutes(45);
    let late_time = base + Duration::hours(1); // beyond stop_after

    let start_period = base.timestamp() as u64;
    let end_period = (base + Duration::hours(1)).timestamp() as u64;

    put_share_at(
        &awsl,
        gateway_reward_share(pubkey.as_ref().to_vec(), 1, 2, 3, start_period, end_period),
        early_time,
    )
    .await?;
    put_share_at(
        &awsl,
        gateway_reward_share(pubkey.as_ref().to_vec(), 4, 5, 6, start_period, end_period),
        late_time,
    )
    .await?;

    let opts = test_backfill_options("rewards-backfill-stop-after", start_time, stop_time);
    run_backfill(pool, awsl.bucket_client(), writer.clone(), opts).await?;
    writer.flush_all().await?;
    tasks.abort_all();

    let gateways: Vec<GatewayRewardRow> = get_all_or_empty(
        harness.trino(),
        format!("SELECT * FROM {}.{}", NAMESPACE, gateway_reward::TABLE_NAME),
    )
    .await?;
    assert_eq!(
        gateways.len(),
        1,
        "expected only the early file (the late file is past stop_after)"
    );
    assert_eq!(gateways[0].beacon_amount, 1);

    awsl.cleanup().await?;
    Ok(())
}

#[sqlx::test]
async fn backfill_skips_records_with_empty_oneof(pool: PgPool) -> anyhow::Result<()> {
    let harness = setup_iceberg().await?;
    let (writer, tasks, _spools) = fanout_writer(&harness).await?;

    let awsl = AwsLocal::new().await;
    awsl.create_bucket().await?;

    let base = Utc::now() - Duration::hours(1);
    let start_time = base - Duration::minutes(1);
    let end_time = base + Duration::days(1);

    let pubkey: PublicKeyBinary = "112NqN2WWMwtK29PMzRby62fDydBJfsCLkCAf392stdok48ovNT6".parse()?;
    let start_period = base.timestamp() as u64;
    let end_period = (base + Duration::hours(1)).timestamp() as u64;
    let empty = IotRewardShare {
        start_period,
        end_period,
        reward: None,
    };
    let real = gateway_reward_share(pubkey.as_ref().to_vec(), 1, 1, 1, start_period, end_period);

    awsl.put_protos_at_time(
        FileType::IotRewardShare.to_string(),
        vec![empty, real],
        base,
    )
    .await
    .map_err(|e| anyhow::anyhow!("put proto: {e}"))?;

    let opts = test_backfill_options("rewards-backfill-skip-empty", start_time, end_time);
    run_backfill(pool, awsl.bucket_client(), writer.clone(), opts).await?;
    writer.flush_all().await?;
    tasks.abort_all();

    let gateways: Vec<GatewayRewardRow> = get_all_or_empty(
        harness.trino(),
        format!("SELECT * FROM {}.{}", NAMESPACE, gateway_reward::TABLE_NAME),
    )
    .await?;
    assert_eq!(gateways.len(), 1, "empty oneof should be skipped");

    let ops: Vec<OperationalRewardRow> = get_all_or_empty(
        harness.trino(),
        format!(
            "SELECT * FROM {}.{}",
            NAMESPACE,
            operational_reward::TABLE_NAME
        ),
    )
    .await?;
    assert!(ops.is_empty());

    awsl.cleanup().await?;
    Ok(())
}

/// Build an `IotRewardsFanoutWriter` over the harness's three reward tables.
/// Each inner `BatchedWriter` gets its own per-test `TempDir` spool, returned
/// here so the directories aren't reaped before the tasks exit.
async fn fanout_writer(
    harness: &IcebergTestHarness,
) -> anyhow::Result<(IotRewardsFanoutWriter, FanoutTaskHandles, [TempDir; 3])> {
    let catalog = harness.iceberg_catalog().clone();

    let gateway_table: IcebergTable<IcebergIotGatewayReward> =
        IcebergTable::from_catalog(catalog.clone(), NAMESPACE, gateway_reward::TABLE_NAME).await?;
    let operational_table: IcebergTable<IcebergIotOperationalReward> =
        IcebergTable::from_catalog(catalog.clone(), NAMESPACE, operational_reward::TABLE_NAME)
            .await?;
    let unallocated_table: IcebergTable<IcebergIotUnallocatedReward> =
        IcebergTable::from_catalog(catalog, NAMESPACE, unallocated_reward::TABLE_NAME).await?;

    let g_spool = TempDir::new()?;
    let o_spool = TempDir::new()?;
    let u_spool = TempDir::new()?;

    let (gateway, g_task) =
        BatchedWriter::new(gateway_table, BatchedWriterConfig::new(g_spool.path()));
    let (operational, o_task) =
        BatchedWriter::new(operational_table, BatchedWriterConfig::new(o_spool.path()));
    let (unallocated, u_task) =
        BatchedWriter::new(unallocated_table, BatchedWriterConfig::new(u_spool.path()));

    let writer = IotRewardsFanoutWriter::from_writers(gateway, operational, unallocated);

    let (_trigger, listener) = triggered::trigger();
    let g_join = tokio::spawn(g_task.run(listener.clone()));
    let o_join = tokio::spawn(o_task.run(listener.clone()));
    let u_join = tokio::spawn(u_task.run(listener));
    let tasks = FanoutTaskHandles {
        gateway: g_join,
        operational: o_join,
        unallocated: u_join,
    };

    Ok((writer, tasks, [g_spool, o_spool, u_spool]))
}

struct FanoutTaskHandles {
    gateway: tokio::task::JoinHandle<helium_iceberg::Result<()>>,
    operational: tokio::task::JoinHandle<helium_iceberg::Result<()>>,
    unallocated: tokio::task::JoinHandle<helium_iceberg::Result<()>>,
}

impl FanoutTaskHandles {
    fn abort_all(self) {
        self.gateway.abort();
        self.operational.abort();
        self.unallocated.abort();
    }
}

async fn put_share_at(
    awsl: &AwsLocal,
    share: IotRewardShare,
    at: chrono::DateTime<Utc>,
) -> anyhow::Result<()> {
    awsl.put_protos_at_time(FileType::IotRewardShare.to_string(), vec![share], at)
        .await
        .map_err(|e| anyhow::anyhow!("put proto: {e}"))?;
    Ok(())
}

async fn run_backfill(
    pool: PgPool,
    bucket: file_store::BucketClient,
    writer: IotRewardsFanoutWriter,
    opts: BackfillOptions,
) -> anyhow::Result<()> {
    let (backfiller, server) = IotRewardsBackfiller::create(pool, bucket, writer, opts).await?;
    tokio::time::timeout(
        TEST_TIMEOUT,
        task_manager::TaskManager::builder()
            .add_task(server)
            .add_task(backfiller)
            .build()
            .start(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("backfill timed out after {:?}", TEST_TIMEOUT))??;
    Ok(())
}

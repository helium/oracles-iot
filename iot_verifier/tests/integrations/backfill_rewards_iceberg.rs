use chrono::{Duration, Utc};
use file_store::aws_local::AwsLocal;
use file_store_oracles::FileType;
use helium_crypto::PublicKeyBinary;
use helium_iceberg::{BoxedDataWriter, IcebergTestHarness, IntoBoxedDataWriter};
use helium_proto::services::poc_lora::{IotRewardShare, UnallocatedRewardType};
use iot_verifier::backfill::{
    rewards::{IotRewardRow, IotRewardsBackfiller, IotRewardsFanoutWriter},
    BackfillOptions,
};
use iot_verifier::iceberg::{
    self, gateway_reward, operational_reward, unallocated_reward, IcebergIotGatewayReward,
    IcebergIotOperationalReward, IcebergIotUnallocatedReward,
};
use sqlx::PgPool;
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

async fn fanout_writer(
    harness: &IcebergTestHarness,
) -> anyhow::Result<BoxedDataWriter<IotRewardRow>> {
    let gateway = harness
        .get_table_writer::<IcebergIotGatewayReward>(gateway_reward::TABLE_NAME)
        .await?;
    let operational = harness
        .get_table_writer::<IcebergIotOperationalReward>(operational_reward::TABLE_NAME)
        .await?;
    let unallocated = harness
        .get_table_writer::<IcebergIotUnallocatedReward>(unallocated_reward::TABLE_NAME)
        .await?;
    Ok(IotRewardsFanoutWriter::new(iceberg::RewardWriters {
        gateway,
        operational,
        unallocated,
    })
    .boxed())
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
    writer: BoxedDataWriter<IotRewardRow>,
    opts: BackfillOptions,
) -> anyhow::Result<()> {
    let (backfiller, server) =
        IotRewardsBackfiller::create(pool, bucket, Some(writer), Some(opts)).await?;
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

#[sqlx::test]
async fn backfill_writes_all_three_reward_variants(pool: PgPool) -> anyhow::Result<()> {
    let harness = setup_iceberg().await?;
    let writer = fanout_writer(&harness).await?;

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
    run_backfill(pool, awsl.bucket_client(), writer, opts).await?;

    let gateways: Vec<GatewayRewardRow> = get_all_or_empty(
        harness.trino(),
        format!("SELECT * FROM rewards.{}", gateway_reward::TABLE_NAME),
    )
    .await?;
    assert_eq!(gateways.len(), 1, "expected 1 gateway reward");
    assert_eq!(gateways[0].hotspot_key, pubkey.to_string());
    assert_eq!(gateways[0].beacon_amount, 10);
    assert_eq!(gateways[0].witness_amount, 20);
    assert_eq!(gateways[0].dc_transfer_amount, 30);

    let ops: Vec<OperationalRewardRow> = get_all_or_empty(
        harness.trino(),
        format!("SELECT * FROM rewards.{}", operational_reward::TABLE_NAME),
    )
    .await?;
    assert_eq!(ops.len(), 1, "expected 1 operational reward");
    assert_eq!(ops[0].amount, 7_000);

    let unalloc: Vec<UnallocatedRewardRow> = get_all_or_empty(
        harness.trino(),
        format!("SELECT * FROM rewards.{}", unallocated_reward::TABLE_NAME),
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
    let writer = fanout_writer(&harness).await?;

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
    run_backfill(pool, awsl.bucket_client(), writer, opts).await?;

    let gateways: Vec<GatewayRewardRow> = get_all_or_empty(
        harness.trino(),
        format!("SELECT * FROM rewards.{}", gateway_reward::TABLE_NAME),
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

/// The writer's `helium.write_id` snapshot property is what keeps re-runs over
/// the same source files from duplicating rows in iceberg. Test that directly
/// (the backfiller's own file-state tracking would already short-circuit a
/// double-run before reaching the writer).
#[tokio::test]
async fn fanout_writer_is_idempotent_on_same_id() -> anyhow::Result<()> {
    let harness = setup_iceberg().await?;
    let writer = fanout_writer(&harness).await?;

    let pubkey: PublicKeyBinary = "112NqN2WWMwtK29PMzRby62fDydBJfsCLkCAf392stdok48ovNT6".parse()?;
    let base = Utc::now() - Duration::hours(1);
    let start_period = base.timestamp() as u64;
    let end_period = (base + Duration::hours(1)).timestamp() as u64;

    let row = IotRewardRow::Gateway(gateway_reward::from_proto(
        helium_proto::services::poc_lora::GatewayReward {
            hotspot_key: pubkey.as_ref().to_vec(),
            beacon_amount: 42,
            witness_amount: 42,
            dc_transfer_amount: 42,
        },
        start_period,
        end_period,
    )?);

    let write_id = "rewards/file-key-abc.gz";
    writer.write_idempotent(write_id, vec![row.clone()]).await?;
    writer.write_idempotent(write_id, vec![row]).await?;

    let gateways: Vec<GatewayRewardRow> = get_all_or_empty(
        harness.trino(),
        format!("SELECT * FROM rewards.{}", gateway_reward::TABLE_NAME),
    )
    .await?;
    assert_eq!(
        gateways.len(),
        1,
        "second write with same id should be a no-op"
    );

    Ok(())
}

#[sqlx::test]
async fn backfill_skips_records_with_empty_oneof(pool: PgPool) -> anyhow::Result<()> {
    let harness = setup_iceberg().await?;
    let writer = fanout_writer(&harness).await?;

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
    run_backfill(pool, awsl.bucket_client(), writer, opts).await?;

    let gateways: Vec<GatewayRewardRow> = get_all_or_empty(
        harness.trino(),
        format!("SELECT * FROM rewards.{}", gateway_reward::TABLE_NAME),
    )
    .await?;
    assert_eq!(gateways.len(), 1, "empty oneof should be skipped");

    let ops: Vec<OperationalRewardRow> = get_all_or_empty(
        harness.trino(),
        format!("SELECT * FROM rewards.{}", operational_reward::TABLE_NAME),
    )
    .await?;
    assert!(ops.is_empty());

    awsl.cleanup().await?;
    Ok(())
}

use chrono::{Duration, Utc};
use file_store::aws_local::AwsLocal;
use file_store_oracles::FileType;
use helium_iceberg::{BoxedDataWriter, IcebergTestHarness};
use iot_verifier::backfill::burns::IotBurnsBackfiller;
use iot_verifier::iceberg::{reward_manifest, IcebergIotRewardManifest};
use sqlx::PgPool;
use trino_rust_client::Trino;

use crate::common::iceberg::{
    iot_reward_manifest, mobile_reward_manifest, setup_iceberg, test_backfill_options,
};

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

const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Debug, Clone, Trino, serde::Serialize, serde::Deserialize, PartialEq)]
struct RewardManifestRow {
    epoch: i64,
    start_timestamp: chrono::DateTime<chrono::FixedOffset>,
    end_timestamp: chrono::DateTime<chrono::FixedOffset>,
    price: i64,
    token: String,
    poc_bones_per_beacon_reward_share: String,
    poc_bones_per_witness_reward_share: String,
    dc_bones_per_share: String,
    written_files: Vec<String>,
}

async fn manifest_writer(
    harness: &IcebergTestHarness,
) -> anyhow::Result<BoxedDataWriter<IcebergIotRewardManifest>> {
    let writer = harness
        .get_table_writer::<IcebergIotRewardManifest>(reward_manifest::TABLE_NAME)
        .await?;
    Ok(writer)
}

async fn run_backfill(
    pool: PgPool,
    bucket: file_store::BucketClient,
    writer: BoxedDataWriter<IcebergIotRewardManifest>,
    opts: iot_verifier::backfill::BackfillOptions,
) -> anyhow::Result<()> {
    let (backfiller, server) =
        IotBurnsBackfiller::create(pool, bucket, Some(writer), Some(opts)).await?;
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
async fn backfill_writes_iot_reward_manifests(pool: PgPool) -> anyhow::Result<()> {
    let harness = setup_iceberg().await?;
    let writer = manifest_writer(&harness).await?;

    let awsl = AwsLocal::new().await;
    awsl.create_bucket().await?;

    let base = Utc::now() - Duration::hours(1);
    let start_time = base - Duration::minutes(1);
    let end_time = base + Duration::days(1);

    let manifest_a = iot_reward_manifest(
        100,
        base.timestamp() as u64,
        (base + Duration::hours(1)).timestamp() as u64,
        1_000,
        "1.5",
        "2.5",
        "3.5",
    );
    let manifest_b = iot_reward_manifest(
        101,
        (base + Duration::hours(1)).timestamp() as u64,
        (base + Duration::hours(2)).timestamp() as u64,
        2_000,
        "10",
        "20",
        "30",
    );

    awsl.put_protos_at_time(FileType::RewardManifest.to_string(), vec![manifest_a], base)
        .await
        .map_err(|e| anyhow::anyhow!("put proto: {e}"))?;
    awsl.put_protos_at_time(
        FileType::RewardManifest.to_string(),
        vec![manifest_b],
        base + Duration::seconds(1),
    )
    .await
    .map_err(|e| anyhow::anyhow!("put proto: {e}"))?;

    let opts = test_backfill_options("burns-backfill-iot", start_time, end_time);
    run_backfill(pool, awsl.bucket_client(), writer, opts).await?;

    let mut rows: Vec<RewardManifestRow> = get_all_or_empty(
        harness.trino(),
        format!(
            "SELECT * FROM poc.{} ORDER BY epoch",
            reward_manifest::TABLE_NAME
        ),
    )
    .await?;
    assert_eq!(rows.len(), 2, "expected 2 iot reward manifests");

    let first = rows.remove(0);
    assert_eq!(first.epoch, 100);
    assert_eq!(first.price, 1_000);
    assert_eq!(first.token, "Hnt");
    assert_eq!(first.poc_bones_per_beacon_reward_share, "1.5");
    assert_eq!(first.poc_bones_per_witness_reward_share, "2.5");
    assert_eq!(first.dc_bones_per_share, "3.5");

    let second = rows.remove(0);
    assert_eq!(second.epoch, 101);
    assert_eq!(second.dc_bones_per_share, "30");

    awsl.cleanup().await?;
    Ok(())
}

#[sqlx::test]
async fn backfill_skips_mobile_reward_manifests(pool: PgPool) -> anyhow::Result<()> {
    let harness = setup_iceberg().await?;
    let writer = manifest_writer(&harness).await?;

    let awsl = AwsLocal::new().await;
    awsl.create_bucket().await?;

    let base = Utc::now() - Duration::hours(1);
    let start_time = base - Duration::minutes(1);
    let end_time = base + Duration::days(1);

    let iot = iot_reward_manifest(
        200,
        base.timestamp() as u64,
        (base + Duration::hours(1)).timestamp() as u64,
        500,
        "0.5",
        "0.5",
        "0.5",
    );
    let mobile = mobile_reward_manifest(
        201,
        (base + Duration::hours(1)).timestamp() as u64,
        (base + Duration::hours(2)).timestamp() as u64,
    );

    // Both go in the same file to confirm the converter filters at the row
    // level, not just the file level.
    awsl.put_protos_at_time(
        FileType::RewardManifest.to_string(),
        vec![iot, mobile],
        base,
    )
    .await
    .map_err(|e| anyhow::anyhow!("put proto: {e}"))?;

    let opts = test_backfill_options("burns-backfill-skip-mobile", start_time, end_time);
    run_backfill(pool, awsl.bucket_client(), writer, opts).await?;

    let rows: Vec<RewardManifestRow> = get_all_or_empty(
        harness.trino(),
        format!("SELECT * FROM poc.{}", reward_manifest::TABLE_NAME),
    )
    .await?;
    assert_eq!(rows.len(), 1, "mobile manifest should be filtered out");
    assert_eq!(rows[0].epoch, 200);

    awsl.cleanup().await?;
    Ok(())
}

/// The writer's `helium.write_id` snapshot property guards against duplicate
/// rows when the same source file is replayed. Test that directly (the
/// backfiller's own file-state tracking would short-circuit a double-run
/// before reaching the writer).
#[tokio::test]
async fn manifest_writer_is_idempotent_on_same_id() -> anyhow::Result<()> {
    let harness = setup_iceberg().await?;
    let writer = manifest_writer(&harness).await?;

    let base = Utc::now() - Duration::hours(1);
    let manifest = iot_reward_manifest(
        300,
        base.timestamp() as u64,
        (base + Duration::hours(1)).timestamp() as u64,
        1_000,
        "9.0",
        "9.0",
        "9.0",
    );
    let row = reward_manifest::try_from_iot_manifest(
        file_store_oracles::network_common::reward_manifest::RewardManifest::try_from(manifest)?,
    )
    .expect("iot manifest");

    let write_id = "burns/file-key-xyz.gz";
    writer.write_idempotent(write_id, vec![row.clone()]).await?;
    writer.write_idempotent(write_id, vec![row]).await?;

    let rows: Vec<RewardManifestRow> = get_all_or_empty(
        harness.trino(),
        format!("SELECT * FROM poc.{}", reward_manifest::TABLE_NAME),
    )
    .await?;
    assert_eq!(rows.len(), 1, "second write with same id should be a no-op");

    Ok(())
}

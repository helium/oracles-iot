use chrono::{Duration, Utc};
use file_store::aws_local::AwsLocal;
use file_store_oracles::FileType;
use helium_crypto::PublicKeyBinary;
use helium_iceberg::{
    BatchedWriter, BatchedWriterConfig, BatchedWriterTask, IcebergTable, IcebergTestHarness,
};
use helium_proto::services::packet_verifier::ValidPacket;
use iot_packet_verifier::{
    backfill::{valid_packets::IotValidPacketsBackfiller, BackfillOptions},
    iceberg::{valid_packet, IcebergIotValidPacket, NAMESPACE},
};
use sqlx::PgPool;
use tempfile::TempDir;
use trino_rust_client::Trino;

const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Debug, Clone, Trino, serde::Serialize, serde::Deserialize, PartialEq)]
struct ValidPacketRow {
    gateway: String,
    payload_size: i64,
    payload_hash: String,
    num_dcs: i64,
    packet_timestamp: chrono::DateTime<chrono::FixedOffset>,
}

#[sqlx::test]
async fn backfill_writes_valid_packets(pool: PgPool) -> anyhow::Result<()> {
    let harness = setup_iceberg().await?;
    let (writer, batched_task, _spool) = batched_writer(&harness).await?;

    let awsl = AwsLocal::new().await;
    awsl.create_bucket().await?;

    let pubkey: PublicKeyBinary = "112NqN2WWMwtK29PMzRby62fDydBJfsCLkCAf392stdok48ovNT6".parse()?;
    let base = Utc::now() - Duration::hours(1);
    let start_time = base - Duration::minutes(1);
    let end_time = base + Duration::days(1);

    let pkt_ms = base.timestamp_millis() as u64;
    let pkts = vec![
        valid_packet_proto(&pubkey, 1, pkt_ms),
        valid_packet_proto(&pubkey, 2, pkt_ms + 1_000),
        valid_packet_proto(&pubkey, 3, pkt_ms + 2_000),
    ];

    awsl.put_protos_at_time(FileType::IotValidPacket.to_string(), pkts, base)
        .await
        .map_err(|e| anyhow::anyhow!("put proto: {e}"))?;

    let opts = test_backfill_options("valid-packets-backfill-basic", start_time, end_time);
    run_backfill(pool, awsl.bucket_client(), writer.clone(), opts).await?;
    writer.flush().await?;
    drop(writer);
    batched_task.await??;

    let mut rows: Vec<ValidPacketRow> = get_all_or_empty(
        harness.trino(),
        format!(
            "SELECT * FROM {}.{} ORDER BY num_dcs",
            NAMESPACE,
            valid_packet::TABLE_NAME
        ),
    )
    .await?;
    assert_eq!(rows.len(), 3, "expected 3 valid packets");
    let first = rows.remove(0);
    assert_eq!(first.gateway, pubkey.to_string());
    assert_eq!(first.payload_size, 24);
    assert_eq!(first.payload_hash, "deadbeef");
    assert_eq!(first.num_dcs, 1);
    let totals: i64 = rows.iter().map(|r| r.num_dcs).sum::<i64>() + first.num_dcs;
    assert_eq!(totals, 6, "sum of num_dcs across all rows");

    awsl.cleanup().await?;
    Ok(())
}

#[sqlx::test]
async fn backfill_skips_files_after_stop_after(pool: PgPool) -> anyhow::Result<()> {
    let harness = setup_iceberg().await?;
    let (writer, batched_task, _spool) = batched_writer(&harness).await?;

    let awsl = AwsLocal::new().await;
    awsl.create_bucket().await?;

    let pubkey: PublicKeyBinary = "112NqN2WWMwtK29PMzRby62fDydBJfsCLkCAf392stdok48ovNT6".parse()?;
    let base = Utc::now() - Duration::hours(2);
    let start_time = base - Duration::minutes(1);
    let early_time = base;
    let stop_time = base + Duration::minutes(45);
    let late_time = base + Duration::hours(1);

    awsl.put_protos_at_time(
        FileType::IotValidPacket.to_string(),
        vec![valid_packet_proto(
            &pubkey,
            1,
            base.timestamp_millis() as u64,
        )],
        early_time,
    )
    .await
    .map_err(|e| anyhow::anyhow!("put proto: {e}"))?;
    awsl.put_protos_at_time(
        FileType::IotValidPacket.to_string(),
        vec![valid_packet_proto(
            &pubkey,
            99,
            late_time.timestamp_millis() as u64,
        )],
        late_time,
    )
    .await
    .map_err(|e| anyhow::anyhow!("put proto: {e}"))?;

    let opts = test_backfill_options("valid-packets-backfill-stop-after", start_time, stop_time);
    run_backfill(pool, awsl.bucket_client(), writer.clone(), opts).await?;
    writer.flush().await?;
    drop(writer);
    batched_task.await??;

    let rows: Vec<ValidPacketRow> = get_all_or_empty(
        harness.trino(),
        format!("SELECT * FROM {}.{}", NAMESPACE, valid_packet::TABLE_NAME),
    )
    .await?;
    assert_eq!(
        rows.len(),
        1,
        "expected only the early file (the late file is past stop_after)"
    );
    assert_eq!(rows[0].num_dcs, 1);

    awsl.cleanup().await?;
    Ok(())
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

async fn setup_iceberg() -> anyhow::Result<IcebergTestHarness> {
    let harness = IcebergTestHarness::new_with_tables([valid_packet::table_definition()?]).await?;
    Ok(harness)
}

/// Build a `BatchedWriter` over the harness's already-created
/// `valid_packets` table. Returns the cloneable handle, a JoinHandle for the
/// background task, and the spool `TempDir` (kept alive by the test so the
/// directory isn't reaped before the task exits).
async fn batched_writer(
    harness: &IcebergTestHarness,
) -> anyhow::Result<(
    BatchedWriter<IcebergIotValidPacket>,
    tokio::task::JoinHandle<helium_iceberg::Result<()>>,
    TempDir,
)> {
    let table = IcebergTable::<IcebergIotValidPacket>::from_catalog(
        harness.iceberg_catalog().clone(),
        NAMESPACE,
        valid_packet::TABLE_NAME,
    )
    .await?;

    let spool = TempDir::new()?;
    let (writer, task) = BatchedWriter::new(table, BatchedWriterConfig::new(spool.path()));
    let task: BatchedWriterTask<IcebergIotValidPacket> = task;
    let (_trigger, listener) = triggered::trigger();
    let join = tokio::spawn(task.run(listener));

    Ok((writer, join, spool))
}

fn test_backfill_options(
    process_name: &str,
    start_after: chrono::DateTime<Utc>,
    stop_after: chrono::DateTime<Utc>,
) -> BackfillOptions {
    BackfillOptions {
        process_name: process_name.to_string(),
        start_after,
        stop_after,
        poll_duration: Some(std::time::Duration::from_millis(100)),
        idle_timeout: Some(std::time::Duration::from_millis(500)),
    }
}

fn valid_packet_proto(gateway: &PublicKeyBinary, num_dcs: u32, ts_ms: u64) -> ValidPacket {
    ValidPacket {
        payload_size: 24,
        gateway: gateway.as_ref().to_vec(),
        payload_hash: vec![0xde, 0xad, 0xbe, 0xef],
        num_dcs,
        packet_timestamp: ts_ms,
    }
}

async fn run_backfill(
    pool: PgPool,
    bucket: file_store::BucketClient,
    writer: BatchedWriter<IcebergIotValidPacket>,
    opts: BackfillOptions,
) -> anyhow::Result<()> {
    let (backfiller, server) =
        IotValidPacketsBackfiller::create(pool, bucket, Some(writer), Some(opts)).await?;
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

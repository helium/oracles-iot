//! End-to-end integration tests for the iot_packet_verifier daemon's iceberg
//! live-write path. These tests drive `Daemon::handle_file` directly so the
//! production code does the iceberg flush — the test only sets up the daemon's
//! dependencies and verifies the resulting iceberg state via Trino.

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use file_store::{
    file_info::FileInfo,
    file_info_poller::FileInfoStream,
    file_sink::{FileSinkClient, Message as SinkMessage},
};
use file_store_oracles::iot_packet::PacketRouterPacketReport;
use helium_crypto::PublicKeyBinary;
use helium_iceberg::{IcebergTable, IcebergTestHarness};
use helium_proto::{
    services::{
        packet_verifier::{InvalidPacket, ValidPacket},
        router::packet_router_packet_report_v1::PacketType,
    },
    DataRate, Region,
};
use iot_packet_verifier::{
    balances::BalanceCache,
    daemon::Daemon,
    iceberg::{self, valid_packet, IcebergIotValidPacket},
    verifier::{ConfigServer, ConfigServerError, Org, Verifier},
};
use solana::burn::TestSolanaClientMap;
use sqlx::PgPool;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{mpsc, Mutex};
use trino_rust_client::Trino;

// ── Tests ────────────────────────────────────────────────────────────────────

#[sqlx::test]
async fn handle_file_writes_valid_packets_to_iceberg(pool: PgPool) -> anyhow::Result<()> {
    let harness = setup_iceberg().await?;
    let writer = writer(&harness).await?;

    let payer = PublicKeyBinary::from(vec![0]);
    let orgs = MockConfigServer::default();
    orgs.insert(0_u64, payer.clone()).await;
    let solana = TestSolanaClientMap::default();
    solana.insert(&payer, 1_000).await;
    let balances = BalanceCache::new(&pool, solana).await?;

    let (mut daemon, valid_drainer, invalid_drainer) =
        build_daemon(pool.clone(), balances, orgs, Some(writer)).await?;

    let stream = report_file_stream(
        "iot_valid_packet/test-file-1.gz",
        vec![
            packet_report(0, 1_700_000_000_000, 24, vec![1], false),
            packet_report(0, 1_700_000_001_000, 48, vec![2], false),
            packet_report(0, 1_700_000_002_000, 24, vec![3], false),
        ],
    );

    daemon.handle_file(stream).await?;

    valid_drainer.abort();
    invalid_drainer.abort();

    let rows: Vec<ValidPacketRow> = get_all_or_empty(
        harness.trino(),
        format!(
            "SELECT * FROM {}.{} ORDER BY packet_timestamp",
            iceberg::NAMESPACE,
            valid_packet::TABLE_NAME
        ),
    )
    .await?;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].payload_size, 24);
    assert_eq!(rows[0].payload_hash, "01");
    assert_eq!(rows[1].payload_size, 48);
    assert_eq!(rows[1].payload_hash, "02");
    assert_eq!(rows[2].payload_hash, "03");
    // payload_size 24 → 1 DC (one BYTES_PER_DC bucket).
    assert_eq!(rows[0].num_dcs, 1);

    Ok(())
}

#[sqlx::test]
async fn handle_file_drops_insufficient_balance_packets_from_iceberg(
    pool: PgPool,
) -> anyhow::Result<()> {
    let harness = setup_iceberg().await?;
    let writer = writer(&harness).await?;

    let payer = PublicKeyBinary::from(vec![0]);
    let orgs = MockConfigServer::default();
    orgs.insert(0_u64, payer.clone()).await;
    let solana = TestSolanaClientMap::default();
    // Just enough balance for ONE 24-byte packet (1 DC).
    solana.insert(&payer, 1).await;
    let balances = BalanceCache::new(&pool, solana).await?;

    let (mut daemon, valid_drainer, invalid_drainer) =
        build_daemon(pool.clone(), balances, orgs, Some(writer)).await?;

    let stream = report_file_stream(
        "iot_valid_packet/test-file-2.gz",
        vec![
            // first one uses up the only DC
            packet_report(0, 1_700_000_000_000, 24, vec![1], false),
            // second one has insufficient balance → InvalidPacket
            packet_report(0, 1_700_000_001_000, 24, vec![2], false),
            // free packet → valid with num_dcs=0
            packet_report(0, 1_700_000_002_000, 24, vec![3], true),
        ],
    );

    daemon.handle_file(stream).await?;

    valid_drainer.abort();
    invalid_drainer.abort();

    let rows: Vec<ValidPacketRow> = get_all_or_empty(
        harness.trino(),
        format!(
            "SELECT * FROM {}.{} ORDER BY packet_timestamp",
            iceberg::NAMESPACE,
            valid_packet::TABLE_NAME
        ),
    )
    .await?;
    assert_eq!(rows.len(), 2, "only valid packets land in iceberg");
    assert_eq!(rows[0].num_dcs, 1, "first packet was paid");
    assert_eq!(rows[1].num_dcs, 0, "free packet has 0 num_dcs");

    Ok(())
}

#[sqlx::test]
async fn handle_file_is_idempotent_per_file_key(pool: PgPool) -> anyhow::Result<()> {
    let harness = setup_iceberg().await?;
    let writer = writer(&harness).await?;

    let payer = PublicKeyBinary::from(vec![0]);
    let orgs = MockConfigServer::default();
    orgs.insert(0_u64, payer.clone()).await;
    let solana = TestSolanaClientMap::default();
    solana.insert(&payer, 1_000).await;
    let balances = BalanceCache::new(&pool, solana).await?;

    let (mut daemon, valid_drainer, invalid_drainer) =
        build_daemon(pool.clone(), balances, orgs, Some(writer)).await?;

    let file_key = "iot_valid_packet/idempotent.gz";
    daemon
        .handle_file(report_file_stream(
            file_key,
            vec![packet_report(0, 1_700_000_000_000, 24, vec![0xab], false)],
        ))
        .await?;

    // Second call with the same file_key. The duplicate `INSERT INTO
    // files_processed` will error inside `into_stream` before reaching the
    // iceberg flush — but even if it did reach iceberg, `write_idempotent`
    // would no-op on the matching `helium.write_id`. Either way, only one
    // row should land in iceberg.
    let _ = daemon
        .handle_file(report_file_stream(
            file_key,
            vec![packet_report(0, 1_700_000_000_000, 24, vec![0xab], false)],
        ))
        .await;

    valid_drainer.abort();
    invalid_drainer.abort();

    let rows: Vec<ValidPacketRow> = get_all_or_empty(
        harness.trino(),
        format!(
            "SELECT * FROM {}.{}",
            iceberg::NAMESPACE,
            valid_packet::TABLE_NAME
        ),
    )
    .await?;
    assert_eq!(
        rows.len(),
        1,
        "second handle_file with same file_key should be a no-op at iceberg"
    );

    Ok(())
}

// ── Daemon test rig ──────────────────────────────────────────────────────────

/// Builds a `Daemon` wired for end-to-end testing: real iceberg writer from
/// the harness, mock config server, real `BalanceCache` (with a
/// `TestSolanaClientMap`-backed Solana network), and proto sinks that are
/// drained in the background so `commit()` calls don't deadlock.
async fn build_daemon(
    pool: PgPool,
    balances: BalanceCache<TestSolanaClientMap>,
    orgs: MockConfigServer,
    iceberg_writer: Option<IcebergTable<IcebergIotValidPacket>>,
) -> anyhow::Result<(
    Daemon<BalanceCache<TestSolanaClientMap>, MockConfigServer>,
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<()>,
)> {
    let (valid_tx, valid_rx) = mpsc::channel::<SinkMessage<ValidPacket>>(16);
    let (invalid_tx, invalid_rx) = mpsc::channel::<SinkMessage<InvalidPacket>>(16);
    let valid_packets = FileSinkClient::new(valid_tx, "valid_packets_metric");
    let invalid_packets = FileSinkClient::new(invalid_tx, "invalid_packets_metric");

    let valid_drainer = spawn_sink_drainer(valid_rx);
    let invalid_drainer = spawn_sink_drainer(invalid_rx);

    // `report_files` is unused by `handle_file` (only `Daemon::run` reads it).
    // We construct an empty receiver so the field has a valid value.
    let (_unused_tx, report_files) = mpsc::channel(1);

    let verifier = Verifier {
        debiter: balances,
        config_server: orgs,
    };

    let daemon = Daemon::new(
        pool,
        verifier,
        report_files,
        valid_packets,
        invalid_packets,
        1, // minimum_allowed_balance
        iceberg_writer,
    );

    Ok((daemon, valid_drainer, invalid_drainer))
}

/// Drains a `FileSinkClient` channel forever, replying `Ok(())` to every
/// `Data` message and `Ok(empty manifest)` to every `Commit`/`Rollback`.
/// Without this, `valid_packets.commit().await?` inside `Daemon::handle_file`
/// would deadlock on the unanswered oneshot.
fn spawn_sink_drainer<T: Send + 'static>(
    mut rx: mpsc::Receiver<SinkMessage<T>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            match msg {
                SinkMessage::Data(on_write_tx, _) => {
                    let _ = on_write_tx.send(Ok(()));
                }
                SinkMessage::Commit(on_commit_tx) => {
                    let _ = on_commit_tx.send(Ok(Vec::new()));
                }
                SinkMessage::Rollback(on_rollback_tx) => {
                    let _ = on_rollback_tx.send(Ok(Vec::new()));
                }
            }
        }
    })
}

/// Wraps a `Vec<PacketRouterPacketReport>` in a `FileInfoStream` keyed on
/// `file_key`. The key is what `Daemon::handle_file` passes to
/// `write_idempotent` for iceberg-level dedupe.
fn report_file_stream(
    file_key: &str,
    reports: Vec<PacketRouterPacketReport>,
) -> FileInfoStream<PacketRouterPacketReport> {
    let file_info = FileInfo {
        key: file_key.to_string(),
        prefix: "iot_packet_report".to_string(),
        timestamp: Utc::now(),
        size: 0,
    };
    FileInfoStream::new("iot_valid_packet_test".to_string(), file_info, reports)
}

// ── Mocks (cribbed from tests/integration_tests.rs) ──────────────────────────

struct MockConfig {
    payer: PublicKeyBinary,
    enabled: bool,
}

#[derive(Default, Clone)]
struct MockConfigServer {
    payers: Arc<Mutex<HashMap<u64, MockConfig>>>,
}

impl MockConfigServer {
    async fn insert(&self, oui: u64, payer: PublicKeyBinary) {
        self.payers.lock().await.insert(
            oui,
            MockConfig {
                payer,
                enabled: true,
            },
        );
    }
}

#[async_trait]
impl ConfigServer for MockConfigServer {
    async fn fetch_org(
        &self,
        oui: u64,
        _cache: &mut HashMap<u64, PublicKeyBinary>,
    ) -> Result<PublicKeyBinary, ConfigServerError> {
        Ok(self.payers.lock().await.get(&oui).unwrap().payer.clone())
    }

    async fn disable_org(&self, oui: u64) -> Result<(), ConfigServerError> {
        self.payers.lock().await.get_mut(&oui).unwrap().enabled = false;
        Ok(())
    }

    async fn enable_org(&self, oui: u64) -> Result<(), ConfigServerError> {
        self.payers.lock().await.get_mut(&oui).unwrap().enabled = true;
        Ok(())
    }

    async fn list_orgs(&self) -> Result<Vec<Org>, ConfigServerError> {
        Ok(self
            .payers
            .lock()
            .await
            .iter()
            .map(|(oui, config)| Org {
                oui: *oui,
                payer: config.payer.clone(),
                locked: !config.enabled,
            })
            .collect())
    }
}

fn packet_report(
    oui: u64,
    timestamp_ms: u64,
    payload_size: u32,
    payload_hash: Vec<u8>,
    free: bool,
) -> PacketRouterPacketReport {
    PacketRouterPacketReport {
        received_timestamp: Utc.timestamp_millis_opt(timestamp_ms as i64).unwrap(),
        oui,
        net_id: 0,
        rssi: 0,
        free,
        frequency: 0,
        snr: 0.0,
        data_rate: DataRate::Fsk50,
        region: Region::As9231,
        gateway: PublicKeyBinary::from(vec![]),
        payload_hash,
        payload_size,
        packet_type: PacketType::Uplink,
    }
}

// ── Iceberg harness ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Trino, serde::Serialize, serde::Deserialize, PartialEq)]
struct ValidPacketRow {
    gateway: String,
    payload_size: i64,
    payload_hash: String,
    num_dcs: i64,
    packet_timestamp: chrono::DateTime<chrono::FixedOffset>,
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

/// Build an `IcebergTable` handle over the harness's already-created
/// `valid_packets` table — the daemon calls `write_idempotent` on this
/// directly.
async fn writer(
    harness: &IcebergTestHarness,
) -> anyhow::Result<IcebergTable<IcebergIotValidPacket>> {
    Ok(IcebergTable::<IcebergIotValidPacket>::from_catalog(
        harness.iceberg_catalog().clone(),
        iceberg::NAMESPACE,
        valid_packet::TABLE_NAME,
    )
    .await?)
}

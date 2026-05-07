use crate::{
    balances::BalanceCache,
    burner::Burner,
    iceberg::{
        IcebergIotValidPacket, ValidPacketIcebergWriter, ValidPacketWriter, ValidPacketWriters,
    },
    pending::confirm_pending_txns,
    settings::Settings,
    verifier::{CachedOrgClient, ConfigServer, Debiter, Verifier},
};
use anyhow::{bail, Result};
use file_store::{
    file_info_poller::FileInfoStream, file_sink::FileSinkClient, file_source, file_upload,
};
use file_store_oracles::{
    iot_packet::PacketRouterPacketReport,
    traits::{FileSinkCommitStrategy, FileSinkRollTime, FileSinkWriteExt},
    FileType,
};
use futures_util::TryFutureExt;
use helium_proto::services::packet_verifier::{InvalidPacket, ValidPacket};
use iot_config::client::OrgClient;
use solana::burn::SolanaRpc;
use sqlx::{Pool, Postgres};
use std::sync::Arc;
use task_manager::{ManagedTask, TaskManager};
use tokio::sync::{mpsc::Receiver, Mutex};

pub type SharedCachedOrgClient<T> = Arc<Mutex<CachedOrgClient<T>>>;

pub struct Daemon<D, C> {
    pub pool: Pool<Postgres>,
    pub verifier: Verifier<D, C>,
    pub report_files: Receiver<FileInfoStream<PacketRouterPacketReport>>,
    pub valid_packets: FileSinkClient<ValidPacket>,
    pub invalid_packets: FileSinkClient<InvalidPacket>,
    pub minimum_allowed_balance: u64,
    pub iceberg_writer: Option<ValidPacketWriter>,
}

impl<D, C> ManagedTask for Daemon<D, C>
where
    D: Debiter + Send + Sync + 'static,
    C: ConfigServer,
{
    fn start_task(self: Box<Self>, shutdown: triggered::Listener) -> task_manager::TaskFuture {
        task_manager::spawn(self.run(shutdown))
    }
}

impl<D, C> Daemon<D, C>
where
    D: Debiter + Send + Sync,
    C: ConfigServer,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: Pool<Postgres>,
        verifier: Verifier<D, C>,
        report_files: Receiver<FileInfoStream<PacketRouterPacketReport>>,
        valid_packets: FileSinkClient<ValidPacket>,
        invalid_packets: FileSinkClient<InvalidPacket>,
        minimum_allowed_balance: u64,
        iceberg_writer: Option<ValidPacketWriter>,
    ) -> Self {
        Self {
            pool,
            verifier,
            report_files,
            valid_packets,
            invalid_packets,
            minimum_allowed_balance,
            iceberg_writer,
        }
    }

    pub async fn run(mut self, shutdown: triggered::Listener) -> Result<()> {
        tracing::info!("Starting verifier daemon");
        loop {
            tokio::select! {
                biased;
                _ = shutdown.clone() => break,
                file = self.report_files.recv() => {
                    if let Some(file) = file {
                        self.handle_file(file).await?
                    } else {
                        bail!("Report file stream was dropped")
                    }
                }

            }
        }
        tracing::info!("Stopping verifier daemon");
        Ok(())
    }

    pub async fn handle_file(
        &mut self,
        report_file: FileInfoStream<PacketRouterPacketReport>,
    ) -> Result<()> {
        let file_key = report_file.file_info.key.clone();
        tracing::info!(file = %report_file.file_info, "Verifying file");

        let mut transaction = self.pool.begin().await?;
        let reports = report_file.into_stream(&mut transaction).await?;

        let mut iceberg_buffer: Vec<IcebergIotValidPacket> = Vec::new();
        let mut wrapped_valid = ValidPacketIcebergWriter {
            inner: &mut self.valid_packets,
            iceberg_buffer: &mut iceberg_buffer,
            enabled: self.iceberg_writer.is_some(),
        };

        self.verifier
            .verify(
                self.minimum_allowed_balance,
                &mut transaction,
                reports,
                &mut wrapped_valid,
                &mut self.invalid_packets,
            )
            .await?;
        transaction.commit().await?;
        self.valid_packets.commit().await?;
        self.invalid_packets.commit().await?;

        if let Some(writer) = &self.iceberg_writer {
            writer
                .write_idempotent(&file_key, iceberg_buffer)
                .await
                .map_err(|e| anyhow::anyhow!("writing iceberg valid_packets: {e}"))?;
        }

        Ok(())
    }
}

#[derive(Debug, clap::Args)]
pub struct Cmd {}

impl Cmd {
    pub async fn run(self, settings: Settings) -> Result<()> {
        poc_metrics::start_metrics(&settings.metrics)?;

        // Set up the postgres pool:
        let pool = settings.database.connect(env!("CARGO_PKG_NAME")).await?;
        sqlx::migrate!().run(&pool).await?;

        let solana = if settings.enable_solana_integration {
            let Some(ref solana_settings) = settings.solana else {
                bail!("Missing solana section in settings");
            };
            // Set up the solana RpcClient:
            Some(SolanaRpc::new(solana_settings).await?)
        } else {
            None
        };

        // Set up the balance cache:
        let balances = BalanceCache::new(&pool, solana.clone()).await?;

        // Check if we have any left over pending transactions, and if we
        // do check if they have been confirmed:
        confirm_pending_txns(&pool, &solana, &balances.balances()).await?;

        // Set up the balance burner:
        let burner = Burner::new(
            pool.clone(),
            &balances,
            settings.burn_period,
            solana.clone(),
        );

        let file_store_client = settings.file_store.connect().await;
        let (file_upload, file_upload_server) =
            file_upload::FileUpload::new(file_store_client.clone(), settings.output_bucket.clone())
                .await;

        let store_base_path = std::path::Path::new(&settings.cache);

        // Verified packets:
        let (valid_packets, valid_packets_server) = ValidPacket::file_sink(
            store_base_path,
            file_upload.clone(),
            FileSinkCommitStrategy::Manual,
            FileSinkRollTime::Default,
            env!("CARGO_PKG_NAME"),
        )
        .await?;

        let (invalid_packets, invalid_packets_server) = InvalidPacket::file_sink(
            store_base_path,
            file_upload.clone(),
            FileSinkCommitStrategy::Manual,
            FileSinkRollTime::Default,
            env!("CARGO_PKG_NAME"),
        )
        .await?;

        let org_client = Arc::new(Mutex::new(CachedOrgClient::new(OrgClient::from_settings(
            &settings.iot_config_client,
        )?)));

        let (report_files, report_files_server) = file_source::continuous_source()
            .state(pool.clone())
            .file_store(file_store_client, settings.ingest_bucket.clone())
            .lookback_start_after(settings.start_after)
            .prefix(FileType::IotPacketReport.to_string())
            .create()
            .await?;

        let iceberg_writer = match &settings.iceberg_settings {
            Some(iceberg_settings) => Some(
                ValidPacketWriters::from_settings(iceberg_settings)
                    .await?
                    .valid_packet,
            ),
            None => None,
        };

        let balance_store = balances.balances();
        let verifier_daemon = Daemon::new(
            pool,
            Verifier {
                debiter: balances,
                config_server: org_client.clone(),
            },
            report_files,
            valid_packets,
            invalid_packets,
            settings.minimum_allowed_balance,
            iceberg_writer,
        );

        // Run the services:
        let minimum_allowed_balance = settings.minimum_allowed_balance;
        let monitor_funds_period = settings.monitor_funds_period;

        TaskManager::builder()
            .add_task(file_upload_server)
            .add_task(valid_packets_server)
            .add_task(invalid_packets_server)
            .add_task(move |shutdown| {
                org_client
                    .monitor_funds(
                        solana,
                        balance_store,
                        minimum_allowed_balance,
                        monitor_funds_period,
                        shutdown,
                    )
                    .map_err(task_manager::TaskError::from_err)
            })
            .add_task(verifier_daemon)
            .add_task(burner)
            .add_task(report_files_server)
            .build()
            .start()
            .await?;
        Ok(())
    }
}

use crate::{
    backfill::{
        rewards::{IotRewardsBackfiller, IotRewardsFanoutWriter},
        settings::Settings,
        BackfillOptions,
    },
    iceberg::RewardWriters,
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use helium_iceberg::IntoBoxedDataWriter;
use task_manager::TaskManager;

#[derive(Debug, clap::Args)]
pub struct Cmd {
    /// Process name for tracking iot reward share backfill (avoids conflict with daemon).
    #[clap(long, default_value = "iot-rewards-backfill")]
    process_name: String,

    /// Start processing files after this timestamp.
    /// Format: RFC 3339 (e.g., 2024-01-01T00:00:00Z)
    #[clap(long)]
    start_after: DateTime<Utc>,

    /// Stop processing files when their timestamp is > this value.
    /// Format: RFC 3339 (e.g., 2025-02-25T00:00:00Z)
    #[clap(long)]
    stop_after: DateTime<Utc>,
}

impl Cmd {
    pub async fn run(self, settings: &Settings) -> Result<()> {
        custom_tracing::init(settings.log.clone(), settings.custom_tracing.clone()).await?;

        let pool = settings
            .database
            .connect("iot-verifier-rewards-backfill")
            .await?;
        sqlx::migrate!().run(&pool).await?;

        let iceberg_settings = settings
            .iceberg_settings
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("iceberg_settings required for rewards backfill"))?;

        let writers = RewardWriters::from_settings(iceberg_settings).await?;
        let writer = IotRewardsFanoutWriter::new(writers).boxed();

        tracing::info!(
            process_name = %self.process_name,
            start_after = %self.start_after,
            stop_after = %self.stop_after,
            "starting iot rewards backfill"
        );

        let opts = BackfillOptions {
            process_name: self.process_name,
            start_after: self.start_after,
            stop_after: self.stop_after,
            poll_duration: None,
            idle_timeout: None,
        };

        let (backfiller, server) = IotRewardsBackfiller::create(
            pool,
            settings.ingest_bucket.connect().await,
            Some(writer),
            Some(opts),
        )
        .await?;

        TaskManager::builder()
            .add_task(server)
            .add_task(backfiller)
            .build()
            .start()
            .await?;

        Ok(())
    }
}

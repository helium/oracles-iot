use crate::{
    gateway_cache::GatewayCache, gateway_updater::GatewayUpdater, packet_loader,
    rewarder::Rewarder, telemetry, Settings,
};

use anyhow::Result;
use file_store::{file_source, file_upload};
use file_store_oracles::{
    traits::{FileSinkCommitStrategy, FileSinkRollTime, FileSinkWriteExt},
    FileType,
};
use helium_proto::{
    services::poc_lora::{IotRewardShare, NonRewardablePacket},
    RewardManifest,
};
use iot_config::client::sub_dao_client::SubDaoClient;
use iot_config::client::Client as IotConfigClient;
use price_tracker::PriceTracker;
use std::time::Duration;
use task_manager::TaskManager;

#[derive(Debug, clap::Args)]
pub struct Cmd {}

impl Cmd {
    pub async fn run(&self, settings: &Settings) -> Result<()> {
        custom_tracing::init(settings.log.clone(), settings.custom_tracing.clone()).await?;

        poc_metrics::start_metrics(&settings.metrics)?;
        tracing::info!("Settings: {}", settings.as_json_pretty());

        let pool = settings.database.connect(env!("CARGO_PKG_NAME")).await?;
        sqlx::migrate!().run(&pool).await?;

        telemetry::initialize(&pool).await?;

        let (file_upload, file_upload_server) = file_upload::FileUpload::from_bucket_client(
            settings.file_store_clients.output.connect().await,
        )
        .await;

        let store_base_path = &settings.file_store_clients.cache;

        let iot_config_client = IotConfigClient::from_settings(&settings.iot_config_client)?;
        let sub_dao_rewards_client = SubDaoClient::from_settings(&settings.iot_config_client)?;

        // *
        // gateway cache — still required by packet_loader
        // *
        let (gateway_updater_receiver, gateway_updater_server) =
            GatewayUpdater::new(settings.gateway_refresh_interval, iot_config_client.clone())
                .await?;
        let gateway_cache = GatewayCache::new(gateway_updater_receiver);

        // *
        // price tracker
        // *
        let (price_tracker, price_daemon) = PriceTracker::new(&settings.price_tracker).await?;

        // *
        // rewarder
        // *
        let (rewards_sink, gateway_rewards_sink_server) = IotRewardShare::file_sink(
            store_base_path,
            file_upload.clone(),
            FileSinkCommitStrategy::Manual,
            FileSinkRollTime::Default,
            env!("CARGO_PKG_NAME"),
        )
        .await?;

        let (reward_manifests_sink, reward_manifests_sink_server) = RewardManifest::file_sink(
            store_base_path,
            file_upload.clone(),
            FileSinkCommitStrategy::Manual,
            FileSinkRollTime::Default,
            env!("CARGO_PKG_NAME"),
        )
        .await?;

        let reward_writers = match &settings.iceberg_settings {
            Some(iceberg_settings) => {
                Some(crate::iceberg::RewardWriters::from_settings(iceberg_settings).await?)
            }
            None => None,
        };

        let rewarder = Rewarder::new(
            pool.clone(),
            rewards_sink,
            reward_manifests_sink,
            settings.reward_period,
            settings.reward_period_offset,
            price_tracker,
            sub_dao_rewards_client,
            reward_writers,
        )?;

        // *
        // packet loader (data-transfer DC rewards)
        // *
        let (non_rewardable_packet_sink, non_rewardable_packet_sink_server) =
            NonRewardablePacket::file_sink(
                store_base_path,
                file_upload.clone(),
                FileSinkCommitStrategy::Automatic,
                FileSinkRollTime::Duration(Duration::from_secs(5 * 60)),
                env!("CARGO_PKG_NAME"),
            )
            .await?;

        let max_lookback_age = settings.loader_window_max_lookback_age;
        let packet_interval = settings.packet_interval;
        let (pk_loader_receiver, pk_loader_server) = file_source::continuous_source()
            .state(pool.clone())
            .bucket_client(settings.file_store_clients.packet_input.connect().await)
            .prefix(FileType::IotValidPacket.to_string())
            .lookback_max(max_lookback_age)
            .poll_duration(packet_interval)
            .offset(packet_interval * 2)
            .create()
            .await?;

        let packet_loader = packet_loader::PacketLoader::new(
            pool.clone(),
            gateway_cache,
            pk_loader_receiver,
            non_rewardable_packet_sink,
        );

        TaskManager::builder()
            .add_task(file_upload_server)
            .add_task(gateway_rewards_sink_server)
            .add_task(reward_manifests_sink_server)
            .add_task(non_rewardable_packet_sink_server)
            .add_task(price_daemon)
            .add_task(gateway_updater_server)
            .add_task(packet_loader)
            .add_task(pk_loader_server)
            .add_task(rewarder)
            .build()
            .start()
            .await?;
        Ok(())
    }
}

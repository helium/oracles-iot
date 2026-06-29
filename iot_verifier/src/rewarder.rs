use crate::{
    iceberg::{
        self, gateway_reward, operational_reward, unallocated_reward, IcebergIotGatewayReward,
        IcebergIotOperationalReward, IcebergIotUnallocatedReward,
    },
    resolve_subdao_pubkey,
    reward_share::{self, GatewayShares},
    telemetry, PriceInfo,
};
use chrono::{DateTime, TimeZone, Utc};
use db_store::meta;
use file_store::{file_sink, traits::TimestampEncode};
use helium_proto::{
    reward_manifest::RewardData::IotRewardData,
    services::poc_lora::{
        self as proto, iot_reward_share::Reward as ProtoReward, UnallocatedReward,
        UnallocatedRewardType,
    },
    IotRewardData as ManifestIotRewardData, IotRewardToken, RewardManifest,
};
use humantime_serde::re::humantime;
use iot_config::{
    client::{sub_dao_client::SubDaoEpochRewardInfoResolver, ClientError},
    sub_dao_epoch_reward_info::EpochRewardInfo,
    EpochInfo,
};
use price_tracker::PriceProvider;
use reward_scheduler::Scheduler;
use rust_decimal::prelude::*;
use solana::{SolPubkey, Token};
use sqlx::{PgExecutor, PgPool, Pool, Postgres};
use std::{ops::Range, time::Duration};
use task_manager::ManagedTask;
use tokio::time::sleep;

const REWARDS_NOT_CURRENT_DELAY_PERIOD: Duration = Duration::from_secs(5 * 60);

/// Per-table buffer of Iceberg rows produced during a single rewarding epoch.
/// All three tables share the same `helium.write_id` (`rewards-epoch-{day}`)
/// so re-running the same epoch is idempotent at the iceberg layer.
#[derive(Default)]
pub struct RewardRowAccumulator {
    pub gateway: Vec<IcebergIotGatewayReward>,
    pub operational: Vec<IcebergIotOperationalReward>,
    pub unallocated: Vec<IcebergIotUnallocatedReward>,
}

pub struct Rewarder<A, P> {
    sub_dao: SolPubkey,
    pub pool: Pool<Postgres>,
    pub rewards_sink: file_sink::FileSinkClient<proto::IotRewardShare>,
    pub reward_manifests_sink: file_sink::FileSinkClient<RewardManifest>,
    pub reward_period_hours: Duration,
    pub reward_offset: Duration,
    pub price_tracker: P,
    sub_dao_epoch_reward_client: A,
    reward_writers: Option<iceberg::RewardWriters>,
}

impl<A, P> ManagedTask for Rewarder<A, P>
where
    A: SubDaoEpochRewardInfoResolver<Error = ClientError> + Send + Sync + 'static,
    P: PriceProvider + Send + Sync + 'static,
{
    fn start_task(self: Box<Self>, shutdown: triggered::Listener) -> task_manager::TaskFuture {
        task_manager::spawn(self.run(shutdown))
    }
}

impl<A, P> Rewarder<A, P>
where
    A: SubDaoEpochRewardInfoResolver<Error = ClientError> + Send + Sync + 'static,
    P: PriceProvider + Send + Sync + 'static,
{
    #[expect(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        rewards_sink: file_sink::FileSinkClient<proto::IotRewardShare>,
        reward_manifests_sink: file_sink::FileSinkClient<RewardManifest>,
        reward_period_hours: Duration,
        reward_offset: Duration,
        price_tracker: P,
        sub_dao_epoch_reward_client: A,
        reward_writers: Option<iceberg::RewardWriters>,
    ) -> anyhow::Result<Self> {
        let sub_dao = resolve_subdao_pubkey();
        tracing::info!("Iot SubDao pubkey: {}", sub_dao);
        Ok(Self {
            sub_dao,
            pool,
            rewards_sink,
            reward_manifests_sink,
            reward_period_hours,
            reward_offset,
            price_tracker,
            sub_dao_epoch_reward_client,
            reward_writers,
        })
    }

    pub async fn run(mut self, shutdown: triggered::Listener) -> anyhow::Result<()> {
        tracing::info!("Starting rewarder");

        loop {
            let next_reward_epoch = next_reward_epoch(&self.pool).await?;
            let next_reward_epoch_period = EpochInfo::from(next_reward_epoch);

            let scheduler = Scheduler::new(
                self.reward_period_hours,
                next_reward_epoch_period.period.start,
                next_reward_epoch_period.period.end,
                self.reward_offset,
            );

            let now = Utc::now();
            let sleep_duration = if scheduler.should_trigger(now) {
                if self.data_current_check(&scheduler.schedule_period).await? {
                    match self.reward(next_reward_epoch).await {
                        Ok(()) => {
                            tracing::info!("Successfully rewarded for epoch {}", next_reward_epoch);
                            scheduler.sleep_duration(Utc::now())?
                        }
                        Err(e) => {
                            tracing::error!("Failed to reward: {}", e);
                            REWARDS_NOT_CURRENT_DELAY_PERIOD
                        }
                    }
                } else {
                    REWARDS_NOT_CURRENT_DELAY_PERIOD
                }
            } else {
                scheduler.sleep_duration(Utc::now())?
            };

            tracing::info!(
                "rewards will be rerun in {}",
                humantime::format_duration(sleep_duration)
            );

            let shutdown = shutdown.clone();
            tokio::select! {
                biased;
                _ = shutdown => break,
                _ = sleep(sleep_duration) => (),
            }
        }

        tracing::info!("Stopping rewarder");
        Ok(())
    }

    pub async fn reward(&mut self, next_reward_epoch: u64) -> anyhow::Result<()> {
        tracing::info!(
            "Resolving reward info for epoch: {}, subdao: {}",
            next_reward_epoch,
            self.sub_dao
        );

        let reward_info = self
            .sub_dao_epoch_reward_client
            .resolve_info(&self.sub_dao.to_string(), next_reward_epoch)
            .await?
            .ok_or(anyhow::anyhow!(
                "No reward info found for epoch {}",
                next_reward_epoch
            ))?;

        let pricer_hnt_price = self
            .price_tracker
            .price(&helium_proto::BlockchainTokenTypeV1::Hnt)
            .await?;

        let price_info = PriceInfo::new(pricer_hnt_price, Token::Hnt.decimals());

        tracing::info!(
            "Rewarding for epoch {} period: {} to {} with hnt bone price: {} and reward pool: {}",
            reward_info.epoch_day,
            reward_info.epoch_period.start,
            reward_info.epoch_period.end,
            price_info.price_per_bone,
            reward_info.epoch_emissions,
        );

        let mut iceberg_rows = self
            .reward_writers
            .is_some()
            .then(RewardRowAccumulator::default);

        // process data transfer rewards; returns the DC underflow for the ops fund
        // and the per-share rate used (for the reward manifest)
        let (dc_underflow, dc_bones_per_share) = reward_dc(
            &self.pool,
            &self.rewards_sink,
            &reward_info,
            price_info.clone(),
            iceberg_rows.as_mut(),
        )
        .await?;

        // operations fund absorbs its base (37%) plus any DC underflow
        reward_operational(
            &self.rewards_sink,
            &reward_info,
            dc_underflow,
            iceberg_rows.as_mut(),
        )
        .await?;

        reward_oracles(&self.rewards_sink, &reward_info, iceberg_rows.as_mut()).await?;

        let written_files = self.rewards_sink.commit().await?.await??;

        if let (Some(writers), Some(rows)) = (self.reward_writers.as_ref(), iceberg_rows) {
            let write_id = format!("rewards-epoch-{}", reward_info.epoch_day);
            writers
                .gateway
                .write_idempotent(&write_id, rows.gateway)
                .await?;
            writers
                .operational
                .write_idempotent(&write_id, rows.operational)
                .await?;
            writers
                .unallocated
                .write_idempotent(&write_id, rows.unallocated)
                .await?;
        }

        let mut transaction = self.pool.begin().await?;

        GatewayShares::clear_rewarded_shares(&mut transaction, reward_info.epoch_period.end)
            .await?;

        save_next_reward_epoch(&mut *transaction, reward_info.epoch_day + 1).await?;

        transaction.commit().await?;

        let reward_data = ManifestIotRewardData {
            // PoC retired (HIP-0149) — no beacon/witness rewards are emitted.
            poc_bones_per_beacon_reward_share: None,
            poc_bones_per_witness_reward_share: None,
            dc_bones_per_share: Some(helium_proto::Decimal {
                value: dc_bones_per_share.to_string(),
            }),
            token: IotRewardToken::Hnt as i32,
        };
        self.reward_manifests_sink
            .write(
                RewardManifest {
                    start_timestamp: reward_info.epoch_period.start.encode_timestamp(),
                    end_timestamp: reward_info.epoch_period.end.encode_timestamp(),
                    written_files,
                    reward_data: Some(IotRewardData(reward_data)),
                    epoch: reward_info.epoch_day,
                    price: price_info.price_in_bones,
                },
                [],
            )
            .await?
            .await??;
        self.reward_manifests_sink.commit().await?;
        telemetry::last_rewarded_end_time(reward_info.epoch_period.end);
        Ok(())
    }

    async fn data_current_check(
        &self,
        reward_period: &Range<DateTime<Utc>>,
    ) -> anyhow::Result<bool> {
        if reward_period.end >= self.disable_complete_data_checks_until().await? {
            if sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM gateway_dc_shares WHERE reward_timestamp >= $1",
            )
            .bind(reward_period.end)
            .fetch_one(&self.pool)
            .await?
                == 0
            {
                tracing::info!("No gateway_dc_shares found past reward period");
                return Ok(false);
            }
        } else {
            tracing::info!("data validity checks are disabled for this reward period");
        }
        Ok(true)
    }

    async fn disable_complete_data_checks_until(&self) -> db_store::Result<DateTime<Utc>> {
        Utc.timestamp_opt(
            meta::fetch(&self.pool, "disable_complete_data_checks_until").await?,
            0,
        )
        .single()
        .ok_or(db_store::Error::DecodeError)
    }
}

/// Distribute data-transfer rewards. Returns the amount of the DC reward
/// allocation that was not consumed (underflow), which the caller passes to
/// `reward_operational` for absorption into the Operations Fund.
pub async fn reward_dc(
    pool: &Pool<Postgres>,
    rewards_sink: &file_sink::FileSinkClient<proto::IotRewardShare>,
    reward_info: &EpochRewardInfo,
    price_info: PriceInfo,
    mut iceberg_rows: Option<&mut RewardRowAccumulator>,
) -> anyhow::Result<(Decimal, Decimal)> {
    let reward_shares =
        reward_share::aggregate_reward_shares(pool, &reward_info.epoch_period).await?;
    let gateway_shares = GatewayShares::new(reward_shares);
    let dc_transfer_rewards_per_share = gateway_shares
        .calculate_rewards_per_share(reward_info.epoch_emissions, price_info)
        .await?;

    let total_dc_rewards = reward_share::get_scheduled_dc_tokens(reward_info.epoch_emissions);

    let start_period_secs = reward_info.epoch_period.start.encode_timestamp();
    let end_period_secs = reward_info.epoch_period.end.encode_timestamp();

    let mut allocated_dc_rewards = 0_u64;
    for (gateway_reward_amount, reward_share) in
        gateway_shares.into_reward_shares(&reward_info.epoch_period, dc_transfer_rewards_per_share)
    {
        if let Some(rows) = iceberg_rows.as_mut() {
            if let Some(ProtoReward::GatewayReward(ref gw)) = reward_share.reward {
                if let Ok(row) =
                    gateway_reward::from_proto(gw.clone(), start_period_secs, end_period_secs)
                {
                    rows.gateway.push(row);
                }
            }
        }
        rewards_sink.write(reward_share, []).await?.await??;
        allocated_dc_rewards += gateway_reward_amount;
    }

    // DC underflow = scheduled DC allocation minus what was actually distributed
    let dc_underflow = (total_dc_rewards - Decimal::from(allocated_dc_rewards))
        .max(Decimal::ZERO)
        .round_dp_with_strategy(0, RoundingStrategy::ToZero);

    tracing::info!(
        %total_dc_rewards,
        %allocated_dc_rewards,
        %dc_underflow,
        "data transfer rewards complete"
    );

    Ok((dc_underflow, dc_transfer_rewards_per_share))
}

pub async fn reward_operational(
    rewards_sink: &file_sink::FileSinkClient<proto::IotRewardShare>,
    reward_info: &EpochRewardInfo,
    dc_underflow: Decimal,
    mut iceberg_rows: Option<&mut RewardRowAccumulator>,
) -> anyhow::Result<()> {
    let total_operational_rewards =
        reward_share::get_scheduled_ops_fund_tokens(reward_info.epoch_emissions, dc_underflow);
    let allocated_operational_rewards = total_operational_rewards
        .round_dp_with_strategy(0, RoundingStrategy::ToZero)
        .to_u64()
        .unwrap_or(0);
    let op_fund_reward = proto::OperationalReward {
        amount: allocated_operational_rewards,
    };
    let start_period_secs = reward_info.epoch_period.start.encode_timestamp();
    let end_period_secs = reward_info.epoch_period.end.encode_timestamp();
    if let Some(rows) = iceberg_rows.as_mut() {
        if let Ok(row) =
            operational_reward::from_proto(op_fund_reward, start_period_secs, end_period_secs)
        {
            rows.operational.push(row);
        }
    }
    rewards_sink
        .write(
            proto::IotRewardShare {
                start_period: start_period_secs,
                end_period: end_period_secs,
                reward: Some(ProtoReward::OperationalReward(op_fund_reward)),
            },
            [],
        )
        .await?
        .await??;
    Ok(())
}

pub async fn reward_oracles(
    rewards_sink: &file_sink::FileSinkClient<proto::IotRewardShare>,
    reward_info: &EpochRewardInfo,
    iceberg_rows: Option<&mut RewardRowAccumulator>,
) -> anyhow::Result<()> {
    // atm 100% of oracle rewards are assigned to 'unallocated'
    let total_oracle_rewards =
        reward_share::get_scheduled_oracle_tokens(reward_info.epoch_emissions);
    let allocated_oracle_rewards = 0_u64;
    let unallocated_oracle_reward_amount = (total_oracle_rewards
        - Decimal::from(allocated_oracle_rewards))
    .round_dp_with_strategy(0, RoundingStrategy::ToZero)
    .to_u64()
    .unwrap_or(0);
    write_unallocated_reward(
        rewards_sink,
        UnallocatedRewardType::Oracle,
        unallocated_oracle_reward_amount,
        &reward_info.epoch_period,
        iceberg_rows,
    )
    .await?;
    Ok(())
}

async fn write_unallocated_reward(
    rewards_sink: &file_sink::FileSinkClient<proto::IotRewardShare>,
    unallocated_type: UnallocatedRewardType,
    unallocated_amount: u64,
    reward_period: &Range<DateTime<Utc>>,
    iceberg_rows: Option<&mut RewardRowAccumulator>,
) -> anyhow::Result<()> {
    if unallocated_amount > 0 {
        let unallocated = UnallocatedReward {
            reward_type: unallocated_type as i32,
            amount: unallocated_amount,
        };
        let start_period_secs = reward_period.start.encode_timestamp();
        let end_period_secs = reward_period.end.encode_timestamp();
        if let Some(rows) = iceberg_rows {
            if let Ok(row) =
                unallocated_reward::from_proto(unallocated, start_period_secs, end_period_secs)
            {
                rows.unallocated.push(row);
            }
        }
        let unallocated_reward = proto::IotRewardShare {
            start_period: start_period_secs,
            end_period: end_period_secs,
            reward: Some(ProtoReward::UnallocatedReward(unallocated)),
        };
        rewards_sink.write(unallocated_reward, []).await?.await??;
    };
    Ok(())
}

pub async fn next_reward_epoch(db: &Pool<Postgres>) -> db_store::Result<u64> {
    meta::fetch(db, "next_reward_epoch").await
}

async fn save_next_reward_epoch(exec: impl PgExecutor<'_>, value: u64) -> db_store::Result<()> {
    meta::store(exec, "next_reward_epoch", value).await
}

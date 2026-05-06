use chrono::{DateTime, Utc};
use helium_iceberg::IcebergTestHarness;
use helium_proto::services::poc_lora::{
    iot_reward_share::Reward as IotReward, GatewayReward, IotRewardShare, OperationalReward,
    UnallocatedReward, UnallocatedRewardType,
};
use iot_verifier::{
    backfill::BackfillOptions,
    iceberg::{gateway_reward, operational_reward, unallocated_reward},
};

/// Create a Polaris-backed test harness with all three IOT reward iceberg tables.
pub async fn setup_iceberg() -> anyhow::Result<IcebergTestHarness> {
    let harness = IcebergTestHarness::new_with_tables([
        gateway_reward::table_definition()?,
        operational_reward::table_definition()?,
        unallocated_reward::table_definition()?,
    ])
    .await?;
    Ok(harness)
}

pub fn test_backfill_options(
    process_name: &str,
    start_after: DateTime<Utc>,
    stop_after: DateTime<Utc>,
) -> BackfillOptions {
    BackfillOptions {
        process_name: process_name.to_string(),
        start_after,
        stop_after,
        poll_duration: Some(std::time::Duration::from_millis(100)),
        idle_timeout: Some(std::time::Duration::from_millis(500)),
    }
}

// ── Reward share helpers ─────────────────────────────────────────────────────

pub fn gateway_reward_share(
    hotspot_key: Vec<u8>,
    beacon_amount: u64,
    witness_amount: u64,
    dc_transfer_amount: u64,
    start_period: u64,
    end_period: u64,
) -> IotRewardShare {
    IotRewardShare {
        start_period,
        end_period,
        reward: Some(IotReward::GatewayReward(GatewayReward {
            hotspot_key,
            beacon_amount,
            witness_amount,
            dc_transfer_amount,
        })),
    }
}

pub fn operational_reward_share(amount: u64, start_period: u64, end_period: u64) -> IotRewardShare {
    IotRewardShare {
        start_period,
        end_period,
        reward: Some(IotReward::OperationalReward(OperationalReward { amount })),
    }
}

pub fn unallocated_reward_share(
    reward_type: UnallocatedRewardType,
    amount: u64,
    start_period: u64,
    end_period: u64,
) -> IotRewardShare {
    IotRewardShare {
        start_period,
        end_period,
        reward: Some(IotReward::UnallocatedReward(UnallocatedReward {
            reward_type: reward_type as i32,
            amount,
        })),
    }
}

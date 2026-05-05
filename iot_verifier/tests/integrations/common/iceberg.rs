use chrono::{DateTime, Utc};
use helium_iceberg::IcebergTestHarness;
use helium_proto::{
    reward_manifest::RewardData as ProtoRewardData,
    services::poc_lora::{
        iot_reward_share::Reward as IotReward, GatewayReward, IotRewardShare, OperationalReward,
        UnallocatedReward, UnallocatedRewardType,
    },
    Decimal as ProtoDecimal, IotRewardData as ProtoIotRewardData, IotRewardToken, MobileRewardData,
    MobileRewardToken, RewardManifest,
};
use iot_verifier::{
    backfill::BackfillOptions,
    iceberg::{gateway_reward, operational_reward, reward_manifest, unallocated_reward},
};

/// Create a Polaris-backed test harness with all four IOT iceberg tables.
pub async fn setup_iceberg() -> anyhow::Result<IcebergTestHarness> {
    let harness = IcebergTestHarness::new_with_tables([
        gateway_reward::table_definition()?,
        operational_reward::table_definition()?,
        unallocated_reward::table_definition()?,
        reward_manifest::table_definition()?,
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

// ── Reward manifest helpers ──────────────────────────────────────────────────

pub fn iot_reward_manifest(
    epoch: u64,
    start_timestamp: u64,
    end_timestamp: u64,
    price: u64,
    poc_beacon: &str,
    poc_witness: &str,
    dc: &str,
) -> RewardManifest {
    RewardManifest {
        written_files: vec![format!("test_file_epoch_{epoch}.gz")],
        start_timestamp,
        end_timestamp,
        epoch,
        price,
        reward_data: Some(ProtoRewardData::IotRewardData(ProtoIotRewardData {
            poc_bones_per_beacon_reward_share: Some(ProtoDecimal {
                value: poc_beacon.to_string(),
            }),
            poc_bones_per_witness_reward_share: Some(ProtoDecimal {
                value: poc_witness.to_string(),
            }),
            dc_bones_per_share: Some(ProtoDecimal {
                value: dc.to_string(),
            }),
            token: IotRewardToken::Hnt as i32,
        })),
    }
}

pub fn mobile_reward_manifest(
    epoch: u64,
    start_timestamp: u64,
    end_timestamp: u64,
) -> RewardManifest {
    RewardManifest {
        written_files: vec![format!("mobile_test_file_epoch_{epoch}.gz")],
        start_timestamp,
        end_timestamp,
        epoch,
        price: 0,
        reward_data: Some(ProtoRewardData::MobileRewardData(MobileRewardData {
            poc_bones_per_reward_share: Some(ProtoDecimal {
                value: "1.0".to_string(),
            }),
            boosted_poc_bones_per_reward_share: Some(ProtoDecimal {
                value: "1.0".to_string(),
            }),
            service_provider_promotions: vec![],
            token: MobileRewardToken::Hnt as i32,
        })),
    }
}

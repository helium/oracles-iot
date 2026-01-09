use crate::indexer::{RewardKey, RewardType};

use anyhow::Result;
use helium_crypto::PublicKeyBinary;
pub mod proto {
    pub use helium_proto::{
        services::{
            poc_lora::{iot_reward_share::Reward as IotReward, IotRewardShare},
            poc_mobile::{mobile_reward_share::Reward as MobileReward, MobileRewardShare},
        },
        IotRewardToken, MobileRewardToken, ServiceProvider,
    };
}

#[derive(thiserror::Error, Debug)]
pub enum ExtractError {
    #[error("invalid iot reward share")]
    InvalidRewardShare,
    #[error("unsupported reward type: {0}")]
    UnsupportedType(&'static str),
    #[error("failed to decode service provider id: {0}")]
    ServiceProviderDecode(i32),
}

pub fn iot_reward(
    share: proto::IotRewardShare,
    op_fund_key: &str,
    unallocated_reward_key: &str,
) -> Result<(RewardKey, u64), ExtractError> {
    let Some(reward) = share.reward else {
        return Err(ExtractError::InvalidRewardShare);
    };

    use proto::IotReward;

    match reward {
        IotReward::GatewayReward(r) => Ok((
            RewardKey {
                key: PublicKeyBinary::from(r.hotspot_key).to_string(),
                reward_type: RewardType::IotGateway,
            },
            r.witness_amount + r.beacon_amount + r.dc_transfer_amount,
        )),
        IotReward::OperationalReward(r) => Ok((
            RewardKey {
                key: op_fund_key.to_string(),
                reward_type: RewardType::IotOperational,
            },
            r.amount,
        )),
        IotReward::UnallocatedReward(r) => Ok((
            RewardKey {
                key: unallocated_reward_key.to_string(),
                reward_type: RewardType::IotUnallocated,
            },
            r.amount,
        )),
    }
}
#[cfg(test)]
mod tests {

    use crate::indexer::RewardType;

    use super::*;

    use chrono::Utc;
    use helium_proto::services::poc_lora::GatewayReward as IotGatewayReward;

    #[test]
    fn test_extract_iot_reward() -> anyhow::Result<()> {
        let reward = proto::IotRewardShare {
            start_period: Utc::now().timestamp_millis() as u64,
            end_period: Utc::now().timestamp_millis() as u64,
            reward: Some(proto::IotReward::GatewayReward(IotGatewayReward {
                hotspot_key: vec![1],
                beacon_amount: 1,
                witness_amount: 2,
                dc_transfer_amount: 3,
            })),
        };

        let (reward_key, amount) = iot_reward(reward, "op-fund-key", "unallocated-key")?;
        assert_eq!(reward_key.key, PublicKeyBinary::from(vec![1]).to_string());
        assert_eq!(reward_key.reward_type, RewardType::IotGateway);
        assert_eq!(amount, 6, "all reward added together");

        Ok(())
    }
}

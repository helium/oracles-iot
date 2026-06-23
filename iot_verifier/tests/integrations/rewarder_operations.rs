use crate::common::{self, rewards_info_24_hours, MockFileSinkReceiver};
use helium_proto::services::poc_lora::{IotRewardShare, OperationalReward};
use iot_verifier::{reward_share, rewarder};
use rust_decimal::{prelude::ToPrimitive, Decimal, RoundingStrategy};
use rust_decimal_macros::dec;

#[tokio::test]
async fn test_operations() -> anyhow::Result<()> {
    let (iot_rewards_client, mut iot_rewards) = common::create_file_sink();

    let reward_info = rewards_info_24_hours();

    let (_, rewards) = tokio::join!(
        rewarder::reward_operational(&iot_rewards_client, &reward_info, dec!(0), None),
        receive_expected_rewards(&mut iot_rewards)
    );
    if let Ok(ops_reward) = rewards {
        // confirm the total rewards allocated matches expectations
        let expected_total =
            reward_share::get_scheduled_ops_fund_tokens(reward_info.epoch_emissions, dec!(0))
                .to_u64()
                .unwrap();
        assert_eq!(ops_reward.amount, 32_945_205_479_452);
        assert_eq!(ops_reward.amount, expected_total);

        // confirm the ops percentage amount matches expectations
        let ops_percent = (Decimal::from(ops_reward.amount) / reward_info.epoch_emissions)
            .round_dp_with_strategy(2, RoundingStrategy::MidpointNearestEven);
        assert_eq!(ops_percent, dec!(0.37));
    } else {
        panic!("no rewards received");
    };
    Ok(())
}

#[tokio::test]
async fn test_operations_with_dc_underflow() -> anyhow::Result<()> {
    let (iot_rewards_client, mut iot_rewards) = common::create_file_sink();

    let reward_info = rewards_info_24_hours();
    // Simulate a full DC underflow: no DC was spent at all, so the entire 50%
    // DC allocation flows to the Ops Fund.
    let dc_underflow = reward_share::get_scheduled_dc_tokens(reward_info.epoch_emissions);

    let (_, rewards) = tokio::join!(
        rewarder::reward_operational(&iot_rewards_client, &reward_info, dc_underflow, None),
        receive_expected_rewards(&mut iot_rewards)
    );
    if let Ok(ops_reward) = rewards {
        let expected_total =
            reward_share::get_scheduled_ops_fund_tokens(reward_info.epoch_emissions, dc_underflow)
                .to_u64()
                .unwrap();
        // 37% base + 50% DC underflow = 87% of epoch emissions
        assert_eq!(ops_reward.amount, expected_total);
        let ops_percent = (Decimal::from(ops_reward.amount) / reward_info.epoch_emissions)
            .round_dp_with_strategy(2, RoundingStrategy::MidpointNearestEven);
        assert_eq!(ops_percent, dec!(0.87));
    } else {
        panic!("no rewards received");
    };
    Ok(())
}

async fn receive_expected_rewards(
    iot_rewards: &mut MockFileSinkReceiver<IotRewardShare>,
) -> anyhow::Result<OperationalReward> {
    // expect one operational reward msg
    let reward = iot_rewards.receive_operational_reward().await;

    // should be no further msgs
    iot_rewards.assert_no_messages();

    Ok(reward)
}

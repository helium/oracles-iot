use crate::common::{self, default_price_info, rewards_info_24_hours, MockFileSinkReceiver};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use helium_proto::services::poc_lora::{GatewayReward, IotRewardShare};
use iot_verifier::{
    reward_share::{self, GatewayDCShare},
    rewarder,
};
use prost::Message;
use rust_decimal::{Decimal, RoundingStrategy};
use rust_decimal_macros::dec;
use sqlx::{PgPool, Postgres, Transaction};

const HOTSPOT_1: &str = "112NqN2WWMwtK29PMzRby62fDydBJfsCLkCAf392stdok48ovNT6";
const HOTSPOT_2: &str = "11uJHS2YaEWJqgqC7yza9uvSmpv5FWoMQXiP8WbxBGgNUmifUJf";

#[sqlx::test]
async fn test_dc_rewards(pool: PgPool) -> anyhow::Result<()> {
    let (iot_rewards_client, mut iot_rewards) = common::create_file_sink();

    let reward_info = rewards_info_24_hours();
    let price_info = default_price_info();

    let mut txn = pool.clone().begin().await?;
    seed_dc(reward_info.epoch_period.start, &mut txn).await?;
    txn.commit().await?;

    let (dc_underflow, gateway_rewards) = tokio::join!(
        rewarder::reward_dc(&pool, &iot_rewards_client, &reward_info, price_info, None),
        receive_expected_rewards(&mut iot_rewards)
    );

    let dc_underflow = dc_underflow?;
    let gateway_rewards = gateway_rewards?;

    assert_eq!(
        gateway_rewards[0].hotspot_key,
        helium_crypto::PublicKeyBinary::from_str(HOTSPOT_1)
            .unwrap()
            .as_ref()
            .to_vec()
    );
    assert_eq!(gateway_rewards[0].beacon_amount, 0);
    assert_eq!(gateway_rewards[0].witness_amount, 0);
    assert_eq!(gateway_rewards[0].dc_transfer_amount, 14_840_182_648_401);

    assert_eq!(
        gateway_rewards[1].hotspot_key,
        helium_crypto::PublicKeyBinary::from_str(HOTSPOT_2)
            .unwrap()
            .as_ref()
            .to_vec()
    );
    assert_eq!(gateway_rewards[1].beacon_amount, 0);
    assert_eq!(gateway_rewards[1].witness_amount, 0);
    assert_eq!(gateway_rewards[1].dc_transfer_amount, 29_680_365_296_803);

    // hotspot2 has double the dc shares so ≈ 2× the dc reward
    assert_eq!(
        gateway_rewards[1].dc_transfer_amount / gateway_rewards[0].dc_transfer_amount,
        2
    );

    // confirm total rewards match expectations
    let dc_sum: u64 = gateway_rewards.iter().map(|r| r.dc_transfer_amount).sum();
    let expected_dc = reward_share::get_scheduled_dc_tokens(reward_info.epoch_emissions);
    // allocated + underflow == scheduled DC budget
    assert_eq!(
        Decimal::from(dc_sum) + dc_underflow,
        expected_dc.round_dp_with_strategy(0, RoundingStrategy::ToZero)
    );

    // dc percentage of epoch emissions (allocated + underflow ≈ 50%)
    let dc_percent = (expected_dc / reward_info.epoch_emissions)
        .round_dp_with_strategy(2, RoundingStrategy::MidpointNearestEven);
    assert_eq!(dc_percent, dec!(0.50));

    Ok(())
}

async fn receive_expected_rewards(
    iot_rewards: &mut MockFileSinkReceiver<IotRewardShare>,
) -> anyhow::Result<Vec<GatewayReward>> {
    let gateway_reward1 = iot_rewards.receive_gateway_reward().await;
    let gateway_reward2 = iot_rewards.receive_gateway_reward().await;

    iot_rewards.assert_no_messages();

    let mut gateway_rewards = vec![gateway_reward1, gateway_reward2];
    gateway_rewards.sort_by(|a, b| b.hotspot_key.cmp(&a.hotspot_key));
    Ok(gateway_rewards)
}

async fn seed_dc(ts: DateTime<Utc>, txn: &mut Transaction<'_, Postgres>) -> anyhow::Result<()> {
    GatewayDCShare {
        hotspot_key: HOTSPOT_1.to_string().parse().unwrap(),
        reward_timestamp: ts + ChronoDuration::hours(1),
        num_dcs: dec!(1000),
        id: "dc_id_1".to_string().encode_to_vec(),
    }
    .save(txn)
    .await?;
    GatewayDCShare {
        hotspot_key: HOTSPOT_2.to_string().parse().unwrap(),
        reward_timestamp: ts + ChronoDuration::hours(1),
        num_dcs: dec!(2000),
        id: "dc_id_2".to_string().encode_to_vec(),
    }
    .save(txn)
    .await?;
    Ok(())
}

use std::str::FromStr;

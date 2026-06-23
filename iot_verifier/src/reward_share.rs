use crate::PriceInfo;
use chrono::{DateTime, Utc};
use file_store::traits::TimestampEncode;
use file_store_oracles::iot_packet::IotValidPacket;
use futures::stream::TryStreamExt;
use helium_crypto::PublicKeyBinary;
use helium_proto::services::poc_lora as proto;
use helium_proto::services::poc_lora::iot_reward_share::Reward as ProtoReward;
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use sqlx::{Postgres, Transaction};
use std::{collections::HashMap, ops::Range};

const DEFAULT_PREC: u32 = 15;

// Oracle-emitted reward buckets (sum to 94% of epoch_emissions):
//   DC 50% + Ops 37% + Oracles 7% = 94%
// The remaining 6% ("Routing") is allocated at the on-chain sub-dao level
// and is never emitted by this oracle — this was true before and after HIP-0149.
static DATA_TRANSFER_REWARDS_PER_DAY_PERCENT: Decimal = dec!(0.50);
// Operations Fund absorbs the former POC bucket (6% beacon + 24% witness = 30%) plus its own 7%
static OPERATIONS_REWARDS_PER_DAY_PERCENT: Decimal = dec!(0.37);
// Oracles fund is allocated 7% of daily rewards
static ORACLES_REWARDS_PER_DAY_PERCENT: Decimal = dec!(0.07);
static DC_USD_PRICE: Decimal = dec!(0.00001);

pub fn get_scheduled_dc_tokens(epoch_emissions: Decimal) -> Decimal {
    epoch_emissions * DATA_TRANSFER_REWARDS_PER_DAY_PERCENT
}

/// Returns the Operations Fund allocation: 37% base + any DC-transfer underflow.
pub fn get_scheduled_ops_fund_tokens(
    epoch_emissions: Decimal,
    dc_transfer_remainder: Decimal,
) -> Decimal {
    epoch_emissions * OPERATIONS_REWARDS_PER_DAY_PERCENT + dc_transfer_remainder
}

pub fn get_scheduled_oracle_tokens(epoch_emissions: Decimal) -> Decimal {
    epoch_emissions * ORACLES_REWARDS_PER_DAY_PERCENT
}

#[derive(sqlx::FromRow)]
pub struct GatewayDCShare {
    pub hotspot_key: PublicKeyBinary,
    pub reward_timestamp: DateTime<Utc>,
    pub num_dcs: Decimal,
    pub id: Vec<u8>,
}

#[derive(sqlx::FromRow)]
struct GatewayShareSaveResult {
    inserted: bool,
}

#[derive(thiserror::Error, Debug)]
#[error(transparent)]
pub struct SaveGatewayShareError(#[from] sqlx::Error);

impl GatewayDCShare {
    pub async fn save(
        self,
        db: &mut Transaction<'_, Postgres>,
    ) -> Result<bool, SaveGatewayShareError> {
        Ok(sqlx::query_as::<_, GatewayShareSaveResult>(
            r#"
            insert into gateway_dc_shares (hotspot_key, reward_timestamp, num_dcs, id)
            values ($1, $2, $3, $4)
            on conflict (id) do update set
                reward_timestamp = EXCLUDED.reward_timestamp,
                num_dcs = EXCLUDED.num_dcs
            returning (xmax = 0) as inserted;
            "#,
        )
        .bind(self.hotspot_key)
        .bind(self.reward_timestamp)
        .bind(self.num_dcs)
        .bind(self.id)
        .fetch_one(&mut **db)
        .await?
        .inserted)
    }

    pub fn share_from_packet(packet: &IotValidPacket) -> Self {
        Self {
            hotspot_key: packet.gateway.clone(),
            reward_timestamp: packet.packet_timestamp,
            num_dcs: Decimal::new(packet.num_dcs as i64, 0),
            id: packet.packet_id(),
        }
    }
}

#[derive(Default)]
pub struct RewardShares {
    pub dc_shares: Decimal,
}

impl RewardShares {
    pub fn add_dc_reward(&mut self, share: &GatewayDCShare) {
        self.dc_shares += share.num_dcs
    }
}

pub type GatewayRewardShares = HashMap<PublicKeyBinary, RewardShares>;

#[derive(Default)]
pub struct GatewayShares {
    pub shares: GatewayRewardShares,
}

impl GatewayShares {
    pub fn new(shares: GatewayRewardShares) -> Self {
        Self { shares }
    }

    pub async fn clear_rewarded_shares(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        period_end: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("delete from gateway_dc_shares where reward_timestamp <= $1")
            .bind(period_end)
            .execute(&mut **tx)
            .await
            .map(|_| ())
    }

    pub fn into_reward_shares(
        self,
        reward_period: &Range<DateTime<Utc>>,
        dc_transfer_rewards_per_share: Decimal,
    ) -> impl Iterator<Item = (u64, proto::IotRewardShare)> + '_ {
        self.shares
            .into_iter()
            .map(move |(hotspot_key, reward_shares)| {
                let dc_transfer_amount =
                    compute_rewards(dc_transfer_rewards_per_share, reward_shares.dc_shares);
                proto::GatewayReward {
                    hotspot_key: hotspot_key.into(),
                    beacon_amount: 0,
                    witness_amount: 0,
                    dc_transfer_amount,
                }
            })
            .filter(|reward_share| reward_share.dc_transfer_amount > 0)
            .map(|gateway_reward| {
                let total_gateway_reward = gateway_reward.dc_transfer_amount;
                (
                    total_gateway_reward,
                    proto::IotRewardShare {
                        start_period: reward_period.start.encode_timestamp(),
                        end_period: reward_period.end.encode_timestamp(),
                        reward: Some(ProtoReward::GatewayReward(gateway_reward)),
                    },
                )
            })
    }

    pub async fn calculate_rewards_per_share(
        &self,
        epoch_emissions: Decimal,
        price_info: PriceInfo,
    ) -> anyhow::Result<Decimal> {
        let total_dc_shares = self.total_dc_shares();
        let total_dc_transfer_rewards = get_scheduled_dc_tokens(epoch_emissions);
        let total_dc_transfer_rewards_used =
            dc_to_hnt_bones(total_dc_shares, price_info.price_per_bone);
        let (_, total_dc_transfer_rewards_capped) = normalize_dc_transfer_rewards(
            total_dc_transfer_rewards_used,
            total_dc_transfer_rewards,
        );
        let dc_transfer_rewards_per_share =
            rewards_per_share(total_dc_transfer_rewards_capped, total_dc_shares);

        tracing::info!(
            %total_dc_shares,
            %total_dc_transfer_rewards_used,
            %dc_transfer_rewards_per_share,
            "data transfer rewards"
        );
        Ok(dc_transfer_rewards_per_share)
    }

    pub fn total_dc_shares(&self) -> Decimal {
        self.shares
            .iter()
            .fold(Decimal::ZERO, |acc, (_, reward_shares)| {
                acc + reward_shares.dc_shares
            })
    }
}

/// Returns the equivalent amount of Hnt bones for a specified amount of Data Credits
pub fn dc_to_hnt_bones(dc_amount: Decimal, hnt_bone_price: Decimal) -> Decimal {
    let dc_in_usd = dc_amount * DC_USD_PRICE;
    (dc_in_usd / hnt_bone_price)
        .round_dp_with_strategy(DEFAULT_PREC, RoundingStrategy::ToPositiveInfinity)
}

pub fn normalize_dc_transfer_rewards(
    total_dc_transfer_rewards_used: Decimal,
    total_dc_transfer_rewards: Decimal,
) -> (Decimal, Decimal) {
    match total_dc_transfer_rewards_used <= total_dc_transfer_rewards {
        true => (
            total_dc_transfer_rewards - total_dc_transfer_rewards_used,
            total_dc_transfer_rewards_used,
        ),
        false => (Decimal::ZERO, total_dc_transfer_rewards),
    }
}

fn rewards_per_share(total_rewards: Decimal, total_shares: Decimal) -> Decimal {
    if total_shares > Decimal::ZERO {
        (total_rewards / total_shares)
            .round_dp_with_strategy(DEFAULT_PREC, RoundingStrategy::MidpointNearestEven)
    } else {
        Decimal::ZERO
    }
}

fn compute_rewards(rewards_per_share: Decimal, shares: Decimal) -> u64 {
    (rewards_per_share * shares)
        .round_dp_with_strategy(0, RoundingStrategy::ToZero)
        .to_u64()
        .unwrap_or(0)
}

pub async fn aggregate_reward_shares(
    db: impl sqlx::PgExecutor<'_> + Copy,
    reward_period: &Range<DateTime<Utc>>,
) -> Result<GatewayRewardShares, sqlx::Error> {
    let mut shares = GatewayRewardShares::default();
    aggregate_dc_shares(&mut shares, db, reward_period).await?;
    Ok(shares)
}

async fn aggregate_dc_shares(
    shares: &mut GatewayRewardShares,
    db: impl sqlx::PgExecutor<'_> + Copy,
    reward_period: &Range<DateTime<Utc>>,
) -> Result<(), sqlx::Error> {
    let mut rows = sqlx::query_as::<_, GatewayDCShare>(
        "select hotspot_key, reward_timestamp, num_dcs::numeric, id from gateway_dc_shares where reward_timestamp > $1 and reward_timestamp <= $2",
    )
    .bind(reward_period.start)
    .bind(reward_period.end)
    .fetch(db);
    while let Some(gateway_share) = rows.try_next().await? {
        shares
            .entry(gateway_share.hotspot_key.clone())
            .or_default()
            .add_dc_reward(&gateway_share)
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::PriceInfo;
    use chrono::Duration;
    use iot_config::sub_dao_epoch_reward_info::EpochRewardInfo;
    use solana::Token;

    pub const EPOCH_ADDRESS: &str = "112E7TxoNHV46M6tiPA8N1MkeMeQxc9ztb4JQLXBVAAUfq1kJLoF";
    pub const SUB_DAO_ADDRESS: &str = "112NqN2WWMwtK29PMzRby62fDydBJfsCLkCAf392stdok48ovNT6";

    const EMISSIONS_POOL_IN_BONES_10_MINUTES: u64 = 618_340_943_683;

    fn reward_shares_in_dec(dc_shares: Decimal) -> RewardShares {
        RewardShares {
            dc_shares: dc_shares
                .round_dp_with_strategy(DEFAULT_PREC, RoundingStrategy::MidpointNearestEven),
        }
    }

    /// returns the equiv dc value for a specified hnt bones amount
    pub fn hnt_bones_to_dc(hnt_amount: Decimal, hnt_bones_price: Decimal) -> Decimal {
        let value = hnt_amount * hnt_bones_price;
        (value / (DC_USD_PRICE)).round_dp_with_strategy(0, RoundingStrategy::ToNegativeInfinity)
    }

    fn rewards_info_1_hour() -> EpochRewardInfo {
        let now = Utc::now();
        let epoch_duration = Duration::hours(1);
        EpochRewardInfo {
            epoch_day: 1,
            epoch_address: EPOCH_ADDRESS.into(),
            sub_dao_address: SUB_DAO_ADDRESS.into(),
            epoch_period: (now - epoch_duration)..now,
            epoch_emissions: dec!(100_000_000_000_000),
            rewards_issued_at: now,
        }
    }

    fn rewards_info_10_minutes() -> EpochRewardInfo {
        let now = Utc::now();
        let epoch_duration = Duration::minutes(10);
        EpochRewardInfo {
            epoch_day: 1,
            epoch_address: EPOCH_ADDRESS.into(),
            sub_dao_address: SUB_DAO_ADDRESS.into(),
            epoch_period: (now - epoch_duration)..now,
            epoch_emissions: Decimal::from(EMISSIONS_POOL_IN_BONES_10_MINUTES),
            rewards_issued_at: now,
        }
    }

    #[test]
    fn test_dc_scheduled_tokens() {
        let rewards_info = rewards_info_1_hour();
        let v = get_scheduled_dc_tokens(rewards_info.epoch_emissions);
        assert_eq!(dec!(50_000_000_000_000), v);
    }

    #[test]
    fn test_op_fund_scheduled_tokens_no_remainder() {
        let rewards_info = rewards_info_1_hour();
        // 37% base, no DC underflow
        let v = get_scheduled_ops_fund_tokens(rewards_info.epoch_emissions, dec!(0));
        assert_eq!(dec!(37_000_000_000_000), v);
    }

    #[test]
    fn test_op_fund_scheduled_tokens_with_remainder() {
        let rewards_info = rewards_info_1_hour();
        // 37% base + 10T underflow
        let v =
            get_scheduled_ops_fund_tokens(rewards_info.epoch_emissions, dec!(10_000_000_000_000));
        assert_eq!(dec!(47_000_000_000_000), v);
    }

    #[test]
    fn test_oracles_scheduled_tokens() {
        let rewards_info = rewards_info_1_hour();
        let v = get_scheduled_oracle_tokens(rewards_info.epoch_emissions);
        assert_eq!(dec!(7_000_000_000_000), v);
    }

    #[test]
    fn test_price_conversion() {
        let token = Token::Hnt;
        let hnt_dollar_price = dec!(1.0);
        let hnt_price_from_pricer = 100000000_u64;
        let hnt_dollar_bone_price = dec!(0.00000001);

        let hnt_price = PriceInfo::new(hnt_price_from_pricer, token.decimals());

        assert_eq!(hnt_dollar_bone_price, hnt_price.price_per_bone);
        assert_eq!(hnt_price_from_pricer, hnt_price.price_in_bones);
        assert_eq!(hnt_dollar_price, hnt_price.price_per_token);
    }

    #[test]
    fn ensure_correct_conversion_of_bytes_to_bones() {
        assert_eq!(dc_to_hnt_bones(Decimal::from(1), dec!(1.0)), dec!(0.00001));
        assert_eq!(dc_to_hnt_bones(Decimal::from(2), dec!(1.0)), dec!(0.00002));
    }

    #[tokio::test]
    async fn test_reward_share_calculation_fixed_dc_spend() {
        let price_info = PriceInfo::new(3590000, 8);

        let gw1: PublicKeyBinary = "112NqN2WWMwtK29PMzRby62fDydBJfsCLkCAf392stdok48ovNT6"
            .parse()
            .expect("failed gw1 parse");
        let gw2: PublicKeyBinary = "11sctWiP9r5wDJVuDe1Th4XSL2vaawaLLSQF8f8iokAoMAJHxqp"
            .parse()
            .expect("failed gw2 parse");

        let reward_info = rewards_info_10_minutes();
        let total_dc_tokens = get_scheduled_dc_tokens(reward_info.epoch_emissions);

        let gw1_dc_spend = dec!(5000);
        let gw2_dc_spend = dec!(5000);

        let mut shares = HashMap::new();
        shares.insert(gw1.clone(), reward_shares_in_dec(gw1_dc_spend));
        shares.insert(gw2.clone(), reward_shares_in_dec(gw2_dc_spend));

        let gw_shares = GatewayShares::new(shares);
        let dc_rewards_per_share = gw_shares
            .calculate_rewards_per_share(reward_info.epoch_emissions, price_info.clone())
            .await
            .unwrap();

        let mut allocated_dc_rewards = 0_u64;
        for (reward_amount, _) in
            gw_shares.into_reward_shares(&reward_info.epoch_period, dc_rewards_per_share)
        {
            allocated_dc_rewards += reward_amount;
        }

        let total_dc_transfer_rewards_used =
            dc_to_hnt_bones(gw1_dc_spend + gw2_dc_spend, price_info.price_per_bone);
        let data_transfer_diff =
            total_dc_transfer_rewards_used.to_i64().unwrap() - allocated_dc_rewards as i64;
        // rounding losses should be minimal
        assert!(data_transfer_diff.abs() <= 2);

        // ops fund should absorb the DC underflow
        let dc_underflow = total_dc_tokens - Decimal::from(allocated_dc_rewards);
        let ops_tokens = get_scheduled_ops_fund_tokens(reward_info.epoch_emissions, dc_underflow);
        // ops base (37%) + underflow > 37% of emissions
        assert!(ops_tokens >= reward_info.epoch_emissions * dec!(0.37));
    }

    #[test]
    fn test_dc_hnt_conversion() {
        let hnt_bone_price = dec!(0.00000359);
        let dc_amount = dec!(1000000);
        let dc_hnt_amt = dc_to_hnt_bones(dc_amount, hnt_bone_price);
        assert_eq!(dc_hnt_amt, dec!(2785515.320334261838441));

        let hnt_dc_amt = hnt_bones_to_dc(dc_hnt_amt, hnt_bone_price);
        assert_eq!(hnt_dc_amt, dc_amount);
    }
}

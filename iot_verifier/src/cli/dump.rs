use std::path::PathBuf;

use anyhow::Result;
use file_store::{file_source, traits::MsgDecode};
use file_store_oracles::{iot_packet::IotValidPacket, reward_manifest::RewardManifest};
use futures::stream::StreamExt;
use helium_crypto::PublicKey;
use helium_proto::{
    services::poc_lora::{iot_reward_share::Reward as IotReward, IotRewardShare},
    Message, PriceReportV1,
};
use serde_json::json;

/// File types produced or consumed by the iot-verifier that this command can
/// decode. The on-disk files are gzipped, length-delimited protobuf streams;
/// `file_source` transparently decompresses and de-frames them.
#[derive(Debug, Clone, clap::ValueEnum)]
pub enum FileType {
    /// Reward shares emitted by the rewarder (iot_network_reward_shares_v1.*.gz)
    IotRewardShare,
    /// Reward manifest emitted alongside the shares (network_reward_manifest_v1.*.gz)
    RewardManifest,
    /// Price reports written by the price oracle (price_report.*.gz)
    PriceReport,
    /// Valid data-transfer packets consumed by the packet loader (iot_valid_packet.*.gz)
    IotValidPacket,
}

/// Decode a store file and print each record as JSON.
///
/// Example:
///   iot-verifier dump -t iot-reward-share -f iot_network_reward_shares_v1.1782245732014.gz
#[derive(Debug, clap::Args)]
pub struct Cmd {
    /// Type of file to decode
    #[clap(short = 't', value_enum)]
    file_type: FileType,
    /// Path to the (gzipped) store file
    #[clap(short = 'f')]
    in_path: PathBuf,
}

impl Cmd {
    pub async fn run(&self) -> Result<()> {
        let mut file_stream = file_source::source([&self.in_path]);

        while let Some(result) = file_stream.next().await {
            let msg = result?;
            match self.file_type {
                FileType::IotRewardShare => {
                    let share = IotRewardShare::decode(msg)?;
                    let start = share.start_period;
                    let end = share.end_period;
                    match share.reward {
                        Some(IotReward::GatewayReward(reward)) => print_json(&json!({
                            "type": "gateway_reward",
                            "start_period": start,
                            "end_period": end,
                            "hotspot_key": PublicKey::try_from(reward.hotspot_key)?.to_string(),
                            "dc_transfer_amount": reward.dc_transfer_amount,
                            "beacon_amount": reward.beacon_amount,
                            "witness_amount": reward.witness_amount,
                        }))?,
                        Some(IotReward::OperationalReward(reward)) => print_json(&json!({
                            "type": "operational_reward",
                            "start_period": start,
                            "end_period": end,
                            "amount": reward.amount,
                        }))?,
                        Some(IotReward::UnallocatedReward(reward)) => print_json(&json!({
                            "type": "unallocated_reward",
                            "start_period": start,
                            "end_period": end,
                            "unallocated_reward_type": reward.reward_type().as_str_name(),
                            "amount": reward.amount,
                        }))?,
                        None => print_json(&json!({
                            "type": "empty",
                            "start_period": start,
                            "end_period": end,
                        }))?,
                    }
                }
                FileType::RewardManifest => {
                    let manifest = RewardManifest::decode(msg)?;
                    print_json(&manifest)?;
                }
                FileType::PriceReport => {
                    let report = PriceReportV1::decode(msg)?;
                    print_json(&json!({
                        "price": report.price,
                        "timestamp": report.timestamp,
                        "token_type": report.token_type().as_str_name(),
                    }))?;
                }
                FileType::IotValidPacket => {
                    let packet = IotValidPacket::decode(msg)?;
                    print_json(&packet)?;
                }
            }
        }

        Ok(())
    }
}

fn print_json<T: ?Sized + serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

pub mod iceberg;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use file_store::file_sink::{FileSinkClient, Message as SinkMessage};
use helium_proto::{
    services::poc_lora::{
        iot_reward_share::Reward as IotReward, GatewayReward, IotRewardShare, OperationalReward,
        UnallocatedReward,
    },
    BlockchainTokenTypeV1,
};
use iot_config::{
    client::{sub_dao_client::SubDaoEpochRewardInfoResolver, ClientError},
    sub_dao_epoch_reward_info::EpochRewardInfo,
};
use iot_verifier::PriceInfo;
use price_tracker::{PriceProvider, PriceTrackerError};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use solana::Token;
use tokio::time::timeout;

pub const EPOCH_ADDRESS: &str = "112E7TxoNHV46M6tiPA8N1MkeMeQxc9ztb4JQLXBVAAUfq1kJLoF";
pub const SUB_DAO_ADDRESS: &str = "112NqN2WWMwtK29PMzRby62fDydBJfsCLkCAf392stdok48ovNT6";
pub const EMISSIONS_POOL_IN_BONES_24_HOURS: u64 = 89_041_095_890_411;

pub fn rewards_info_24_hours() -> EpochRewardInfo {
    let now = Utc::now();
    let epoch_duration = Duration::hours(24);
    EpochRewardInfo {
        epoch_day: 1,
        epoch_address: EPOCH_ADDRESS.into(),
        sub_dao_address: SUB_DAO_ADDRESS.into(),
        epoch_period: (now - epoch_duration)..now,
        epoch_emissions: Decimal::from(EMISSIONS_POOL_IN_BONES_24_HOURS),
        rewards_issued_at: now,
    }
}

pub fn default_price_info() -> PriceInfo {
    let token = Token::Hnt;
    let price_info = PriceInfo::new(1, token.decimals());
    assert_eq!(price_info.price_per_token, dec!(0.00000001));
    assert_eq!(price_info.price_per_bone, dec!(0.0000000000000001));
    price_info
}

pub fn create_file_sink<T: prost::Message>() -> (FileSinkClient<T>, MockFileSinkReceiver<T>) {
    let (tx, rx) = tokio::sync::mpsc::channel(10);
    (
        FileSinkClient::new(tx, "metric"),
        MockFileSinkReceiver { receiver: rx },
    )
}

/// Drains a `FileSinkClient` channel forever, replying `Ok(())` to every
/// `Data` message and `Ok(empty manifest)` to every `Commit`/`Rollback`.
pub fn spawn_file_sink_drainer<T>(
    mut receiver: MockFileSinkReceiver<T>,
) -> tokio::task::JoinHandle<()>
where
    T: Send + 'static,
{
    tokio::spawn(async move {
        while let Some(msg) = receiver.receiver.recv().await {
            match msg {
                SinkMessage::Data(on_write_tx, _) => {
                    let _ = on_write_tx.send(Ok(()));
                }
                SinkMessage::Commit(on_commit_tx) => {
                    let _ = on_commit_tx.send(Ok(Vec::new()));
                }
                SinkMessage::Rollback(on_rollback_tx) => {
                    let _ = on_rollback_tx.send(Ok(Vec::new()));
                }
            }
        }
    })
}

#[derive(Clone, Debug)]
pub struct TestPriceProvider {
    pub price: u64,
}

impl TestPriceProvider {
    pub fn new(price: u64) -> Self {
        Self { price }
    }
}

#[async_trait]
impl PriceProvider for TestPriceProvider {
    async fn price(&self, _token_type: &BlockchainTokenTypeV1) -> Result<u64, PriceTrackerError> {
        Ok(self.price)
    }
}

#[derive(Clone, Debug)]
pub struct MockSubDaoEpochRewardInfoResolver {
    pub info: EpochRewardInfo,
}

impl MockSubDaoEpochRewardInfoResolver {
    pub fn new(info: EpochRewardInfo) -> Self {
        Self { info }
    }
}

#[async_trait]
impl SubDaoEpochRewardInfoResolver for MockSubDaoEpochRewardInfoResolver {
    type Error = ClientError;

    async fn resolve_info(
        &self,
        _sub_dao: &str,
        _epoch: u64,
    ) -> Result<Option<EpochRewardInfo>, ClientError> {
        Ok(Some(self.info.clone()))
    }
}

pub struct MockFileSinkReceiver<T> {
    pub receiver: tokio::sync::mpsc::Receiver<SinkMessage<T>>,
}

impl<T: std::fmt::Debug> MockFileSinkReceiver<T> {
    pub async fn receive(&mut self) -> Option<T> {
        match timeout(seconds(2), self.receiver.recv()).await {
            Ok(Some(SinkMessage::Data(on_write_tx, msg))) => {
                let _ = on_write_tx.send(Ok(()));
                Some(msg)
            }
            Ok(None) => None,
            Err(e) => panic!("timeout while waiting for message {e:?}"),
            Ok(Some(unexpected_msg)) => {
                println!("ignoring unexpected msg {unexpected_msg:?}");
                None
            }
        }
    }

    pub fn assert_no_messages(&mut self) {
        use tokio::sync::mpsc::error::TryRecvError;
        let Err(TryRecvError::Empty) = self.receiver.try_recv() else {
            panic!("receiver should have been empty")
        };
    }
}

impl MockFileSinkReceiver<IotRewardShare> {
    pub async fn receive_gateway_reward(&mut self) -> GatewayReward {
        match self.receive().await {
            Some(iot_reward) => match iot_reward.reward {
                Some(IotReward::GatewayReward(r)) => r,
                _ => panic!("failed to get gateway reward"),
            },
            None => panic!("failed to receive gateway reward"),
        }
    }

    pub async fn receive_operational_reward(&mut self) -> OperationalReward {
        match self.receive().await {
            Some(iot_reward) => match iot_reward.reward {
                Some(IotReward::OperationalReward(r)) => r,
                _ => panic!("failed to get operational reward"),
            },
            None => panic!("failed to receive operational reward"),
        }
    }

    pub async fn receive_unallocated_reward(&mut self) -> UnallocatedReward {
        match self.receive().await {
            Some(iot_reward) => match iot_reward.reward {
                Some(IotReward::UnallocatedReward(r)) => r,
                _ => panic!("failed to get unallocated reward"),
            },
            None => panic!("failed to receive unallocated reward"),
        }
    }
}

fn seconds(s: u64) -> std::time::Duration {
    std::time::Duration::from_secs(s)
}

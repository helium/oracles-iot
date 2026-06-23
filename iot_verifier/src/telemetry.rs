use crate::rewarder;
use chrono::{DateTime, Utc};
use iot_config::EpochInfo;
use sqlx::{Pool, Postgres};
use std::sync::atomic::{AtomicU64, Ordering};

const PACKET_COUNTER: &str = concat!(env!("CARGO_PKG_NAME"), "_", "packet");
const NON_REWARDABLE_PACKET_COUNTER: &str =
    concat!(env!("CARGO_PKG_NAME"), "_", "non_rewardable_packet");
const LAST_REWARDED_END_TIME: &str = "last_rewarded_end_time";

pub async fn initialize(db: &Pool<Postgres>) -> anyhow::Result<()> {
    let next_reward_epoch = rewarder::next_reward_epoch(db).await?;
    let epoch_period: EpochInfo = next_reward_epoch.into();
    last_rewarded_end_time(epoch_period.period.start);
    Ok(())
}

pub fn count_packets(count: u64) {
    metrics::counter!(PACKET_COUNTER).increment(count);
}

pub fn count_non_rewardable_packets(count: u64) {
    metrics::counter!(NON_REWARDABLE_PACKET_COUNTER).increment(count);
}

pub fn last_rewarded_end_time(datetime: DateTime<Utc>) {
    metrics::gauge!(LAST_REWARDED_END_TIME).set(datetime.timestamp() as f64);
}

#[derive(Default)]
pub struct LoaderMetricTracker {
    packets: AtomicU64,
    non_rewardable_packets: AtomicU64,
}

impl LoaderMetricTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn increment_packets(&self) {
        self.packets.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_non_rewardable_packets(&self) {
        self.non_rewardable_packets.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_metrics(self) {
        let packets = self.packets.into_inner();
        let non_rewardable_packets = self.non_rewardable_packets.into_inner();
        if packets > 0 {
            count_packets(packets);
        }
        if non_rewardable_packets > 0 {
            count_non_rewardable_packets(non_rewardable_packets);
        }
    }
}

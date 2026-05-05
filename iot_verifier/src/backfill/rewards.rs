use crate::backfill::{Backfiller, IcebergBackfill};
use crate::iceberg::{
    self, gateway_reward, operational_reward, unallocated_reward, IcebergIotGatewayReward,
    IcebergIotOperationalReward, IcebergIotUnallocatedReward,
};
use async_trait::async_trait;
use file_store::traits::MsgDecode;
use file_store_oracles::FileType;
use helium_iceberg::{BoxedDataWriter, DataWriter};
use helium_proto::services::poc_lora::{iot_reward_share::Reward as IotReward, IotRewardShare};
use serde::Serialize;

/// Wrapper around `IotRewardShare` so we can implement `MsgDecode` (orphan rules
/// prevent us from impl'ing it on the proto type directly). The wrapped record
/// is dispatched per-variant by [`IotRewardsFanoutWriter`].
#[derive(Debug, Clone)]
pub struct IotRewardShareRecord(pub IotRewardShare);

impl From<IotRewardShare> for IotRewardShareRecord {
    fn from(value: IotRewardShare) -> Self {
        Self(value)
    }
}

impl MsgDecode for IotRewardShareRecord {
    type Msg = IotRewardShare;
}

/// Fan-out row produced by [`IotRewardConverter`]. The `Backfiller` collects a
/// `Vec<IotRewardRow>` per source S3 file and hands it to
/// [`IotRewardsFanoutWriter`], which partitions by variant and writes each
/// table independently with the same `helium.write_id` (the source file key)
/// so re-runs over the same window remain idempotent.
#[derive(Debug, Clone, Serialize)]
pub enum IotRewardRow {
    Gateway(IcebergIotGatewayReward),
    Operational(IcebergIotOperationalReward),
    Unallocated(IcebergIotUnallocatedReward),
}

pub struct IotRewardConverter;

impl IcebergBackfill for IotRewardConverter {
    type FileRecord = IotRewardShareRecord;
    type IcebergRow = IotRewardRow;
    const FILE_TYPE: FileType = FileType::IotRewardShare;

    fn convert(record: IotRewardShareRecord) -> Option<IotRewardRow> {
        let IotRewardShareRecord(share) = record;
        let start = share.start_period;
        let end = share.end_period;
        match share.reward? {
            IotReward::GatewayReward(g) => gateway_reward::from_proto(g, start, end)
                .map(IotRewardRow::Gateway)
                .ok(),
            IotReward::OperationalReward(o) => operational_reward::from_proto(o, start, end)
                .map(IotRewardRow::Operational)
                .ok(),
            IotReward::UnallocatedReward(u) => unallocated_reward::from_proto(u, start, end)
                .map(IotRewardRow::Unallocated)
                .ok(),
        }
    }
}

pub type IotRewardsBackfiller = Backfiller<IotRewardConverter>;

pub struct IotRewardsFanoutWriter {
    gateway: BoxedDataWriter<IcebergIotGatewayReward>,
    operational: BoxedDataWriter<IcebergIotOperationalReward>,
    unallocated: BoxedDataWriter<IcebergIotUnallocatedReward>,
}

impl IotRewardsFanoutWriter {
    pub fn new(writers: iceberg::RewardWriters) -> Self {
        Self {
            gateway: writers.gateway,
            operational: writers.operational,
            unallocated: writers.unallocated,
        }
    }

    fn partition(
        records: Vec<IotRewardRow>,
    ) -> (
        Vec<IcebergIotGatewayReward>,
        Vec<IcebergIotOperationalReward>,
        Vec<IcebergIotUnallocatedReward>,
    ) {
        let mut gateway = Vec::new();
        let mut operational = Vec::new();
        let mut unallocated = Vec::new();
        for row in records {
            match row {
                IotRewardRow::Gateway(r) => gateway.push(r),
                IotRewardRow::Operational(r) => operational.push(r),
                IotRewardRow::Unallocated(r) => unallocated.push(r),
            }
        }
        (gateway, operational, unallocated)
    }
}

#[async_trait]
impl DataWriter<IotRewardRow> for IotRewardsFanoutWriter {
    async fn write(&self, records: Vec<IotRewardRow>) -> helium_iceberg::Result {
        let (gateway, operational, unallocated) = Self::partition(records);
        if !gateway.is_empty() {
            self.gateway.write(gateway).await?;
        }
        if !operational.is_empty() {
            self.operational.write(operational).await?;
        }
        if !unallocated.is_empty() {
            self.unallocated.write(unallocated).await?;
        }
        Ok(())
    }

    async fn write_idempotent(
        &self,
        id: &str,
        records: Vec<IotRewardRow>,
    ) -> helium_iceberg::Result {
        let (gateway, operational, unallocated) = Self::partition(records);
        // Always call write_idempotent on every table so that re-runs over a
        // window with empty variants still record the write_id (otherwise a
        // subsequent run could append empty rows from the same file again).
        self.gateway.write_idempotent(id, gateway).await?;
        self.operational.write_idempotent(id, operational).await?;
        self.unallocated.write_idempotent(id, unallocated).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helium_proto::services::poc_lora::{
        GatewayReward, OperationalReward, UnallocatedReward, UnallocatedRewardType,
    };

    fn share(reward: IotReward) -> IotRewardShareRecord {
        IotRewardShareRecord(IotRewardShare {
            start_period: 1_700_000_000,
            end_period: 1_700_086_400,
            reward: Some(reward),
        })
    }

    #[test]
    fn convert_gateway() {
        let r = IotRewardConverter::convert(share(IotReward::GatewayReward(GatewayReward {
            hotspot_key: vec![1, 2, 3],
            beacon_amount: 10,
            witness_amount: 20,
            dc_transfer_amount: 30,
        })))
        .unwrap();
        assert!(matches!(r, IotRewardRow::Gateway(_)));
    }

    #[test]
    fn convert_operational() {
        let r =
            IotRewardConverter::convert(share(IotReward::OperationalReward(OperationalReward {
                amount: 5,
            })))
            .unwrap();
        assert!(matches!(r, IotRewardRow::Operational(_)));
    }

    #[test]
    fn convert_unallocated() {
        let r =
            IotRewardConverter::convert(share(IotReward::UnallocatedReward(UnallocatedReward {
                reward_type: UnallocatedRewardType::Poc as i32,
                amount: 7,
            })))
            .unwrap();
        assert!(matches!(r, IotRewardRow::Unallocated(_)));
    }

    #[test]
    fn convert_skips_empty_oneof() {
        let empty = IotRewardShareRecord(IotRewardShare {
            start_period: 0,
            end_period: 0,
            reward: None,
        });
        assert!(IotRewardConverter::convert(empty).is_none());
    }

    #[test]
    fn partition_buckets() {
        use chrono::{DateTime, FixedOffset, TimeZone, Utc};

        fn epoch() -> DateTime<FixedOffset> {
            Utc.timestamp_opt(0, 0)
                .single()
                .unwrap()
                .with_timezone(&FixedOffset::east_opt(0).unwrap())
        }

        let rows = vec![
            IotRewardRow::Gateway(IcebergIotGatewayReward {
                hotspot_key: "x".into(),
                beacon_amount: 1,
                witness_amount: 0,
                dc_transfer_amount: 0,
                start_period: epoch(),
                end_period: epoch(),
            }),
            IotRewardRow::Operational(IcebergIotOperationalReward {
                amount: 1,
                start_period: epoch(),
                end_period: epoch(),
            }),
            IotRewardRow::Unallocated(IcebergIotUnallocatedReward {
                reward_type: "Poc".into(),
                amount: 1,
                start_period: epoch(),
                end_period: epoch(),
            }),
        ];
        let (g, o, u) = IotRewardsFanoutWriter::partition(rows);
        assert_eq!(g.len(), 1);
        assert_eq!(o.len(), 1);
        assert_eq!(u.len(), 1);
    }
}

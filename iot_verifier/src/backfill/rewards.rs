use crate::backfill::{BackfillWriter, Backfiller, IcebergBackfill};
use crate::iceberg::{
    self, gateway_reward, operational_reward, unallocated_reward, IcebergIotGatewayReward,
    IcebergIotOperationalReward, IcebergIotUnallocatedReward,
};
use anyhow::Context;
use async_trait::async_trait;
use file_store::traits::MsgDecode;
use file_store_oracles::FileType;
use helium_iceberg::{BatchedWriter, BatchedWriterConfig, BatchedWriterTask, IcebergTable};
use helium_proto::services::poc_lora::{iot_reward_share::Reward as IotReward, IotRewardShare};
use serde::Serialize;
use std::path::Path;

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
/// [`IotRewardsFanoutWriter`], which partitions by variant and queues each
/// table independently into its own `BatchedWriter`.
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

pub type IotRewardsBackfiller = Backfiller<IotRewardConverter, IotRewardsFanoutWriter>;

/// One `BatchedWriter` handle per reward iceberg table. Cloneable — each
/// inner `BatchedWriter<T>` is just a cloneable `mpsc::Sender`.
#[derive(Clone)]
pub struct IotRewardsFanoutWriter {
    gateway: BatchedWriter<IcebergIotGatewayReward>,
    operational: BatchedWriter<IcebergIotOperationalReward>,
    unallocated: BatchedWriter<IcebergIotUnallocatedReward>,
}

/// The three `BatchedWriterTask`s that drain the fanout's spools and commit
/// to iceberg. Returned alongside [`IotRewardsFanoutWriter::from_settings`]
/// so the caller can register them with `TaskManager`.
pub struct IotRewardsFanoutTasks {
    pub gateway: BatchedWriterTask<IcebergIotGatewayReward>,
    pub operational: BatchedWriterTask<IcebergIotOperationalReward>,
    pub unallocated: BatchedWriterTask<IcebergIotUnallocatedReward>,
}

impl IotRewardsFanoutWriter {
    /// Connect to the iceberg catalog, ensure the rewards namespace and
    /// tables exist, then build a `BatchedWriter` over each table. Each
    /// writer's spool lives under `<spool_root>/<table_name>` so the three
    /// tables don't share a single spool file.
    pub async fn from_settings(
        iceberg_settings: &helium_iceberg::Settings,
        spool_root: &Path,
    ) -> anyhow::Result<(Self, IotRewardsFanoutTasks)> {
        tracing::info!("connecting to iceberg catalog for iot rewards backfill");
        let catalog = iceberg_settings
            .connect()
            .await
            .context("connecting to catalog")?;
        catalog
            .create_namespace_if_not_exists(iceberg::NAMESPACE)
            .await
            .context("creating rewards namespace")?;

        let gateway_table: IcebergTable<IcebergIotGatewayReward> = catalog
            .create_table_if_not_exists(gateway_reward::table_definition()?)
            .await
            .context("creating iot_gateway_rewards table")?;
        let operational_table: IcebergTable<IcebergIotOperationalReward> = catalog
            .create_table_if_not_exists(operational_reward::table_definition()?)
            .await
            .context("creating iot_operational_rewards table")?;
        let unallocated_table: IcebergTable<IcebergIotUnallocatedReward> = catalog
            .create_table_if_not_exists(unallocated_reward::table_definition()?)
            .await
            .context("creating iot_unallocated_rewards table")?;

        let (gateway, gateway_task) = BatchedWriter::new(
            gateway_table,
            BatchedWriterConfig::new(spool_root.join(gateway_reward::TABLE_NAME)),
        );
        let (operational, operational_task) = BatchedWriter::new(
            operational_table,
            BatchedWriterConfig::new(spool_root.join(operational_reward::TABLE_NAME)),
        );
        let (unallocated, unallocated_task) = BatchedWriter::new(
            unallocated_table,
            BatchedWriterConfig::new(spool_root.join(unallocated_reward::TABLE_NAME)),
        );

        Ok((
            Self {
                gateway,
                operational,
                unallocated,
            },
            IotRewardsFanoutTasks {
                gateway: gateway_task,
                operational: operational_task,
                unallocated: unallocated_task,
            },
        ))
    }

    /// Construct directly from already-built `BatchedWriter` handles. Used
    /// by tests that wire up the writers against the test harness.
    pub fn from_writers(
        gateway: BatchedWriter<IcebergIotGatewayReward>,
        operational: BatchedWriter<IcebergIotOperationalReward>,
        unallocated: BatchedWriter<IcebergIotUnallocatedReward>,
    ) -> Self {
        Self {
            gateway,
            operational,
            unallocated,
        }
    }

    /// Force an iceberg commit for all three tables. Used by tests to make
    /// rows queryable via Trino without having to wait for the size/time
    /// thresholds to fire.
    pub async fn flush_all(&self) -> helium_iceberg::Result<()> {
        self.gateway.flush().await?;
        self.operational.flush().await?;
        self.unallocated.flush().await?;
        Ok(())
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
impl BackfillWriter<IotRewardRow> for IotRewardsFanoutWriter {
    async fn queue_all(&self, records: Vec<IotRewardRow>) -> anyhow::Result<()> {
        let (gateway, operational, unallocated) = Self::partition(records);
        if !gateway.is_empty() {
            self.gateway
                .queue_all(gateway)
                .await
                .context("queueing iot_gateway_rewards rows")?;
        }
        if !operational.is_empty() {
            self.operational
                .queue_all(operational)
                .await
                .context("queueing iot_operational_rewards rows")?;
        }
        if !unallocated.is_empty() {
            self.unallocated
                .queue_all(unallocated)
                .await
                .context("queueing iot_unallocated_rewards rows")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, FixedOffset, TimeZone, Utc};
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

use anyhow::Context;
use chrono::{DateTime, FixedOffset, TimeZone};
use helium_iceberg::{BoxedDataWriter, IntoBoxedDataWriter};

pub mod gateway_reward;
pub mod operational_reward;
pub mod reward_manifest;
pub mod unallocated_reward;

pub use gateway_reward::IcebergIotGatewayReward;
pub use operational_reward::IcebergIotOperationalReward;
pub use reward_manifest::IcebergIotRewardManifest;
pub use unallocated_reward::IcebergIotUnallocatedReward;

pub const NAMESPACE: &str = "poc";
pub const REWARDS_NAMESPACE: &str = "rewards";

pub type GatewayRewardWriter = BoxedDataWriter<IcebergIotGatewayReward>;
pub type OperationalRewardWriter = BoxedDataWriter<IcebergIotOperationalReward>;
pub type UnallocatedRewardWriter = BoxedDataWriter<IcebergIotUnallocatedReward>;
pub type RewardManifestWriter = BoxedDataWriter<IcebergIotRewardManifest>;

pub struct RewardWriters {
    pub gateway: GatewayRewardWriter,
    pub operational: OperationalRewardWriter,
    pub unallocated: UnallocatedRewardWriter,
}

impl RewardWriters {
    pub async fn from_settings(settings: &helium_iceberg::Settings) -> anyhow::Result<Self> {
        tracing::info!("connecting to iceberg catalog for iot reward backfill");
        let catalog = settings.connect().await.context("connecting to catalog")?;
        catalog
            .create_namespace_if_not_exists(REWARDS_NAMESPACE)
            .await
            .context("creating rewards namespace")?;

        let gateway = catalog
            .create_table_if_not_exists(gateway_reward::table_definition()?)
            .await
            .context("creating iot_gateway_rewards table")?
            .boxed();
        let operational = catalog
            .create_table_if_not_exists(operational_reward::table_definition()?)
            .await
            .context("creating iot_operational_rewards table")?
            .boxed();
        let unallocated = catalog
            .create_table_if_not_exists(unallocated_reward::table_definition()?)
            .await
            .context("creating iot_unallocated_rewards table")?
            .boxed();

        Ok(Self {
            gateway,
            operational,
            unallocated,
        })
    }
}

pub struct BurnsWriters {
    pub reward_manifest: RewardManifestWriter,
}

impl BurnsWriters {
    pub async fn from_settings(settings: &helium_iceberg::Settings) -> anyhow::Result<Self> {
        tracing::info!("connecting to iceberg catalog for iot burns backfill");
        let catalog = settings.connect().await.context("connecting to catalog")?;
        catalog
            .create_namespace_if_not_exists(NAMESPACE)
            .await
            .context("creating poc namespace")?;

        let reward_manifest = catalog
            .create_table_if_not_exists(reward_manifest::table_definition()?)
            .await
            .context("creating iot_reward_manifests table")?
            .boxed();

        Ok(Self { reward_manifest })
    }
}

/// Convert a `DateTime<Tz>` to a `DateTime<FixedOffset>` anchored at UTC, which
/// is the wire shape expected by Iceberg `timestamptz` columns.
pub(crate) fn into_offset<Tz: TimeZone>(ts: DateTime<Tz>) -> DateTime<FixedOffset> {
    ts.with_timezone(&FixedOffset::east_opt(0).expect("UTC offset is valid"))
}

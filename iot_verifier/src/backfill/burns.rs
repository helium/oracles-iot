use crate::backfill::{Backfiller, IcebergBackfill};
use crate::iceberg::{reward_manifest, IcebergIotRewardManifest};
use file_store_oracles::{network_common::reward_manifest::RewardManifest, FileType};

pub struct IotBurnConverter;

impl IcebergBackfill for IotBurnConverter {
    type FileRecord = RewardManifest;
    type IcebergRow = IcebergIotRewardManifest;
    const FILE_TYPE: FileType = FileType::RewardManifest;

    fn convert(record: RewardManifest) -> Option<IcebergIotRewardManifest> {
        reward_manifest::try_from_iot_manifest(record)
    }
}

pub type IotBurnsBackfiller = Backfiller<IotBurnConverter>;

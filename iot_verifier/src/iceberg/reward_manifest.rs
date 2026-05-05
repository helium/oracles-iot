use chrono::{DateTime, FixedOffset};
use file_store_oracles::network_common::reward_manifest::{RewardData, RewardManifest};
use helium_iceberg::{FieldDefinition, PartitionDefinition, SortFieldDefinition, TableDefinition};
use serde::{Deserialize, Serialize};

use super::{into_offset, NAMESPACE};

pub const TABLE_NAME: &str = "iot_reward_manifests";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IcebergIotRewardManifest {
    pub epoch: i64,
    pub start_timestamp: DateTime<FixedOffset>,
    pub end_timestamp: DateTime<FixedOffset>,
    pub price: i64,
    pub token: String,
    pub poc_bones_per_beacon_reward_share: String,
    pub poc_bones_per_witness_reward_share: String,
    pub dc_bones_per_share: String,
    pub written_files: Vec<String>,
}

pub fn table_definition() -> helium_iceberg::Result<TableDefinition> {
    TableDefinition::builder(NAMESPACE, TABLE_NAME)
        .with_fields([
            FieldDefinition::required_long("epoch"),
            FieldDefinition::required_timestamptz("start_timestamp"),
            FieldDefinition::required_timestamptz("end_timestamp"),
            FieldDefinition::required_long("price"),
            FieldDefinition::required_string("token"),
            FieldDefinition::required_string("poc_bones_per_beacon_reward_share"),
            FieldDefinition::required_string("poc_bones_per_witness_reward_share"),
            FieldDefinition::required_string("dc_bones_per_share"),
            FieldDefinition::required_list(
                "written_files",
                helium_iceberg::FieldKind::primitive(helium_iceberg::PrimitiveType::String),
            ),
        ])
        .with_partition(PartitionDefinition::day(
            "start_timestamp",
            "start_timestamp_day",
        ))
        .with_sort_fields([SortFieldDefinition::ascending("epoch")])
        .build()
}

/// Returns `None` for non-IOT manifests (mobile reward manifests are skipped).
pub fn try_from_iot_manifest(manifest: RewardManifest) -> Option<IcebergIotRewardManifest> {
    let RewardManifest {
        written_files,
        start_timestamp,
        end_timestamp,
        reward_data,
        epoch,
        price,
    } = manifest;
    let RewardData::IotRewardData {
        poc_bones_per_beacon_reward_share,
        poc_bones_per_witness_reward_share,
        dc_bones_per_share,
        token,
    } = reward_data?
    else {
        return None;
    };
    Some(IcebergIotRewardManifest {
        epoch: epoch as i64,
        start_timestamp: into_offset(start_timestamp),
        end_timestamp: into_offset(end_timestamp),
        price: price as i64,
        token: format!("{token:?}"),
        poc_bones_per_beacon_reward_share: poc_bones_per_beacon_reward_share.to_string(),
        poc_bones_per_witness_reward_share: poc_bones_per_witness_reward_share.to_string(),
        dc_bones_per_share: dc_bones_per_share.to_string(),
        written_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use helium_proto::IotRewardToken;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    #[test]
    fn iot_manifest_maps_fields() {
        let m = RewardManifest {
            written_files: vec!["a".into(), "b".into()],
            start_timestamp: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            end_timestamp: Utc.timestamp_opt(1_700_086_400, 0).unwrap(),
            reward_data: Some(RewardData::IotRewardData {
                poc_bones_per_beacon_reward_share: dec!(1.5),
                poc_bones_per_witness_reward_share: dec!(2.5),
                dc_bones_per_share: dec!(3.5),
                token: IotRewardToken::Hnt,
            }),
            epoch: 42,
            price: 1_000_000,
        };

        let row = try_from_iot_manifest(m).expect("iot manifest");
        assert_eq!(row.epoch, 42);
        assert_eq!(row.price, 1_000_000);
        assert_eq!(row.token, "Hnt");
        assert_eq!(row.poc_bones_per_beacon_reward_share, "1.5");
        assert_eq!(row.poc_bones_per_witness_reward_share, "2.5");
        assert_eq!(row.dc_bones_per_share, "3.5");
        assert_eq!(row.written_files, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(row.start_timestamp.timestamp(), 1_700_000_000);
        assert_eq!(row.end_timestamp.timestamp(), 1_700_086_400);
    }

    #[test]
    fn mobile_manifest_skipped() {
        let m = RewardManifest {
            written_files: vec![],
            start_timestamp: Utc.timestamp_opt(0, 0).unwrap(),
            end_timestamp: Utc.timestamp_opt(0, 0).unwrap(),
            reward_data: Some(RewardData::MobileRewardData {
                poc_bones_per_reward_share: Decimal::ZERO,
                boosted_poc_bones_per_reward_share: Decimal::ZERO,
                token: helium_proto::MobileRewardToken::Hnt,
            }),
            epoch: 0,
            price: 0,
        };
        assert!(try_from_iot_manifest(m).is_none());
    }
}

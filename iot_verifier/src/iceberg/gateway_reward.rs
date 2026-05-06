use super::into_offset;
use crate::iceberg::NAMESPACE;
use chrono::{DateTime, FixedOffset};
use file_store::traits::{TimestampDecode, TimestampDecodeError};
use helium_crypto::PublicKeyBinary;
use helium_iceberg::{FieldDefinition, PartitionDefinition, SortFieldDefinition, TableDefinition};
use helium_proto::services::poc_lora::GatewayReward;
use serde::{Deserialize, Serialize};

pub const TABLE_NAME: &str = "gateway_rewards";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IcebergIotGatewayReward {
    pub hotspot_key: String,
    pub beacon_amount: i64,
    pub witness_amount: i64,
    pub dc_transfer_amount: i64,
    pub start_period: DateTime<FixedOffset>,
    pub end_period: DateTime<FixedOffset>,
}

pub fn table_definition() -> helium_iceberg::Result<TableDefinition> {
    TableDefinition::builder(NAMESPACE, TABLE_NAME)
        .with_fields([
            FieldDefinition::required_string("hotspot_key"),
            FieldDefinition::required_long("beacon_amount"),
            FieldDefinition::required_long("witness_amount"),
            FieldDefinition::required_long("dc_transfer_amount"),
            FieldDefinition::required_timestamptz("start_period"),
            FieldDefinition::required_timestamptz("end_period"),
        ])
        .with_partition(PartitionDefinition::day("start_period", "start_period_day"))
        .with_sort_fields([
            SortFieldDefinition::ascending("hotspot_key"),
            SortFieldDefinition::ascending("start_period"),
        ])
        .build()
}

pub fn from_proto(
    reward: GatewayReward,
    start_period: u64,
    end_period: u64,
) -> Result<IcebergIotGatewayReward, TimestampDecodeError> {
    Ok(IcebergIotGatewayReward {
        hotspot_key: PublicKeyBinary::from(reward.hotspot_key).to_string(),
        beacon_amount: reward.beacon_amount as i64,
        witness_amount: reward.witness_amount as i64,
        dc_transfer_amount: reward.dc_transfer_amount as i64,
        start_period: into_offset(start_period.to_timestamp()?),
        end_period: into_offset(end_period.to_timestamp()?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_proto_maps_fields() {
        let pubkey: PublicKeyBinary = "112NqN2WWMwtK29PMzRby62fDydBJfsCLkCAf392stdok48ovNT6"
            .parse()
            .unwrap();
        let proto = GatewayReward {
            hotspot_key: Vec::<u8>::from(pubkey.clone()),
            beacon_amount: 100,
            witness_amount: 200,
            dc_transfer_amount: 300,
        };

        let row = from_proto(proto, 1_700_000_000, 1_700_086_400).unwrap();

        assert_eq!(row.hotspot_key, pubkey.to_string());
        assert_eq!(row.beacon_amount, 100);
        assert_eq!(row.witness_amount, 200);
        assert_eq!(row.dc_transfer_amount, 300);
        assert_eq!(row.start_period.timestamp(), 1_700_000_000);
        assert_eq!(row.end_period.timestamp(), 1_700_086_400);
    }
}

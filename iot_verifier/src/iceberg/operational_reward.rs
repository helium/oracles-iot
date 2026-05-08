use super::into_offset;
use crate::iceberg::NAMESPACE;
use chrono::{DateTime, FixedOffset};
use file_store::traits::{TimestampDecode, TimestampDecodeError};
use helium_iceberg::{FieldDefinition, PartitionDefinition, SortFieldDefinition, TableDefinition};
use helium_proto::services::poc_lora::OperationalReward;
use serde::{Deserialize, Serialize};

pub const TABLE_NAME: &str = "operational_rewards";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IcebergIotOperationalReward {
    pub amount: u64,
    pub start_period: DateTime<FixedOffset>,
    pub end_period: DateTime<FixedOffset>,
}

pub fn table_definition() -> helium_iceberg::Result<TableDefinition> {
    TableDefinition::builder(NAMESPACE, TABLE_NAME)
        .with_fields([
            FieldDefinition::required_long("amount"),
            FieldDefinition::required_timestamptz("start_period"),
            FieldDefinition::required_timestamptz("end_period"),
        ])
        .with_partition(PartitionDefinition::day("start_period", "start_period_day"))
        .with_sort_fields([SortFieldDefinition::ascending("start_period")])
        .build()
}

pub fn from_proto(
    reward: OperationalReward,
    start_period: u64,
    end_period: u64,
) -> Result<IcebergIotOperationalReward, TimestampDecodeError> {
    Ok(IcebergIotOperationalReward {
        amount: reward.amount,
        start_period: into_offset(start_period.to_timestamp()?),
        end_period: into_offset(end_period.to_timestamp()?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_proto_maps_fields() {
        let proto = OperationalReward { amount: 5_000 };
        let row = from_proto(proto, 1_700_000_000, 1_700_086_400).unwrap();
        assert_eq!(row.amount, 5_000);
        assert_eq!(row.start_period.timestamp(), 1_700_000_000);
        assert_eq!(row.end_period.timestamp(), 1_700_086_400);
    }
}

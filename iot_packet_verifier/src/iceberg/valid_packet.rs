use chrono::{DateTime, FixedOffset};
use file_store_oracles::iot_packet::IotValidPacket;
use helium_iceberg::{FieldDefinition, PartitionDefinition, SortFieldDefinition, TableDefinition};
use serde::{Deserialize, Serialize};

use super::{into_offset, NAMESPACE};

pub const TABLE_NAME: &str = "valid_packets";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IcebergIotValidPacket {
    pub gateway: String,
    pub payload_size: u64,
    /// Hex-encoded payload hash. Stored as a string (rather than `binary`) so
    /// it round-trips through Trino/SQL queries as readable text.
    pub payload_hash: String,
    pub num_dcs: u64,
    pub packet_timestamp: DateTime<FixedOffset>,
}

pub fn table_definition() -> helium_iceberg::Result<TableDefinition> {
    TableDefinition::builder(NAMESPACE, TABLE_NAME)
        .with_fields([
            FieldDefinition::required_string("gateway"),
            FieldDefinition::required_long("payload_size"),
            FieldDefinition::required_string("payload_hash"),
            FieldDefinition::required_long("num_dcs"),
            FieldDefinition::required_timestamptz("packet_timestamp"),
        ])
        .with_partition(PartitionDefinition::day(
            "packet_timestamp",
            "packet_timestamp_day",
        ))
        .with_sort_fields([
            SortFieldDefinition::ascending("gateway"),
            SortFieldDefinition::ascending("packet_timestamp"),
        ])
        .build()
}

pub fn from_record(record: IotValidPacket) -> IcebergIotValidPacket {
    IcebergIotValidPacket {
        gateway: record.gateway.to_string(),
        payload_size: u64::from(record.payload_size),
        payload_hash: hex::encode(&record.payload_hash),
        num_dcs: u64::from(record.num_dcs),
        packet_timestamp: into_offset(record.packet_timestamp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use helium_crypto::PublicKeyBinary;

    #[test]
    fn from_record_maps_fields() {
        let pubkey: PublicKeyBinary = "112NqN2WWMwtK29PMzRby62fDydBJfsCLkCAf392stdok48ovNT6"
            .parse()
            .unwrap();
        let ts = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();

        let record = IotValidPacket {
            payload_size: 24,
            gateway: pubkey.clone(),
            payload_hash: vec![0xde, 0xad, 0xbe, 0xef],
            num_dcs: 1,
            packet_timestamp: ts,
        };

        let row = from_record(record);

        assert_eq!(row.gateway, pubkey.to_string());
        assert_eq!(row.payload_size, 24);
        assert_eq!(row.payload_hash, "deadbeef");
        assert_eq!(row.num_dcs, 1);
        assert_eq!(row.packet_timestamp.timestamp(), 1_700_000_000);
    }
}

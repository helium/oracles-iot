use crate::backfill::{Backfiller, IcebergBackfill};
use crate::iceberg::{valid_packet, IcebergIotValidPacket};
use file_store_oracles::{iot_packet::IotValidPacket, FileType};

pub struct IotValidPacketConverter;

impl IcebergBackfill for IotValidPacketConverter {
    type FileRecord = IotValidPacket;
    type IcebergRow = IcebergIotValidPacket;
    const FILE_TYPE: FileType = FileType::IotValidPacket;

    fn convert(record: IotValidPacket) -> Option<IcebergIotValidPacket> {
        Some(valid_packet::from_record(record))
    }
}

pub type IotValidPacketsBackfiller = Backfiller<IotValidPacketConverter>;

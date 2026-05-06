use anyhow::Context;
use chrono::{DateTime, FixedOffset, TimeZone};
use helium_iceberg::{BoxedDataWriter, IntoBoxedDataWriter};

pub mod valid_packet;

pub use valid_packet::IcebergIotValidPacket;

pub const NAMESPACE: &str = "iot";

pub type ValidPacketWriter = BoxedDataWriter<IcebergIotValidPacket>;

pub struct ValidPacketWriters {
    pub valid_packet: ValidPacketWriter,
}

impl ValidPacketWriters {
    pub async fn from_settings(settings: &helium_iceberg::Settings) -> anyhow::Result<Self> {
        tracing::info!("connecting to iceberg catalog for iot valid packet backfill");
        let catalog = settings.connect().await.context("connecting to catalog")?;
        catalog
            .create_namespace_if_not_exists(NAMESPACE)
            .await
            .context("creating iot namespace")?;

        let valid_packet = catalog
            .create_table_if_not_exists(valid_packet::table_definition()?)
            .await
            .context("creating iot_valid_packets table")?
            .boxed();

        Ok(Self { valid_packet })
    }
}

/// Convert a `DateTime<Tz>` to a `DateTime<FixedOffset>` anchored at UTC, which
/// is the wire shape expected by Iceberg `timestamptz` columns.
pub(crate) fn into_offset<Tz: TimeZone>(ts: DateTime<Tz>) -> DateTime<FixedOffset> {
    ts.with_timezone(&FixedOffset::east_opt(0).expect("UTC offset is valid"))
}

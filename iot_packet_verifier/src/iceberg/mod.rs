use crate::verifier::PacketWriter;
use anyhow::Context;
use chrono::{DateTime, FixedOffset, TimeZone};
use file_store::file_sink::FileSinkClient;
use file_store_oracles::iot_packet::IotValidPacket;
use helium_iceberg::{BoxedDataWriter, IntoBoxedDataWriter};
use helium_proto::services::packet_verifier::ValidPacket;
use tonic::async_trait;
pub use valid_packet::IcebergIotValidPacket;

pub mod valid_packet;

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

/// Wraps `ValidPacket` file sink so that every emitted packet is
/// also captured as an `IcebergIotValidPacket` in `iceberg_buffer`. The buffer
/// is flushed to the Iceberg writer after each source file is fully processed,
/// keyed on the source file path so re-runs are idempotent.
pub struct ValidPacketIcebergWriter<'a> {
    pub inner: &'a mut FileSinkClient<ValidPacket>,
    pub iceberg_buffer: &'a mut Vec<IcebergIotValidPacket>,
    pub enabled: bool,
}

#[async_trait]
impl PacketWriter<ValidPacket> for ValidPacketIcebergWriter<'_> {
    async fn write(&mut self, packet: ValidPacket) -> Result<(), file_store::Error> {
        if self.enabled {
            // The proto → IotValidPacket conversion only fails on out-of-range
            // timestamps, which would also fail downstream. Emit a single
            // warning and keep the proto write going.
            match IotValidPacket::try_from(packet.clone()) {
                Ok(record) => self.iceberg_buffer.push(valid_packet::from_record(record)),
                Err(e) => tracing::warn!(error = %e, "skipping iceberg row for invalid packet"),
            }
        }
        // Forwards to the existing `PacketWriter for FileSinkClient` impl,
        // which internally calls the inherent `FileSinkClient::write` with an
        // empty metadata array.
        self.inner.write(packet).await?;
        Ok(())
    }
}

/// Convert a `DateTime<Tz>` to a `DateTime<FixedOffset>` anchored at UTC, which
/// is the wire shape expected by Iceberg `timestamptz` columns.
pub(crate) fn into_offset<Tz: TimeZone>(ts: DateTime<Tz>) -> DateTime<FixedOffset> {
    ts.with_timezone(&FixedOffset::east_opt(0).expect("UTC offset is valid"))
}

//! Gateway inventory read from Trino.
//!
//! Replaces the direct connection to the Solana on-chain indexer Postgres (the
//! "metadata DB"). The same facts arrive through the data platform as
//! glacon-rs → Iceberg `chain.iot_hotspot_history` → dbt →
//! [`IOT_HOTSPOT_INVENTORY_TABLE`], refreshed every 15 minutes.
//!
//! Three columns the metadata DB carried have no equivalent here — `gain`,
//! `is_active` and `num_location_asserts` are not in the chain pipeline (not in
//! `iot_hotspot_metadata` in chain_rewardable_entities.proto, so not in glacon and
//! not in dbt). They are written NULL. Only `gain` is served, and
//! `From<Gateway> for IotMetadata` already substitutes `DEFAULT_GAIN` for a NULL.

use chrono::{DateTime, Utc};
use helium_crypto::PublicKeyBinary;
use std::hash::{DefaultHasher, Hasher};
use trino_client::TrinoFromRow;

use crate::gateway::db::Gateway;

/// The on-chain IoT hotspot inventory in production. Fully qualified so the query
/// resolves regardless of the Trino client's default catalog/schema; tests pass an
/// override pointing at their own harness catalog.
pub const IOT_HOTSPOT_INVENTORY_TABLE: &str = "network.chain.iot_hotspot_inventory";

/// Rows fetched per Trino query. `trino_client` has no cursor API — `get_all`
/// buffers the whole result into a `Vec` — and the IoT fleet is order-1M rows, so
/// the inventory is walked in keyset pages rather than materialized at once.
const PAGE_SIZE: usize = 50_000;

/// One row of the inventory. Field names must match the `SELECT ... AS` aliases in
/// [`page_statement`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, TrinoFromRow)]
pub struct InventoryRow {
    pub_key: String,
    /// NULL for a hotspot that has never asserted a location.
    asserted_hex: Option<String>,
    elevation: Option<i32>,
    is_data_only: Option<bool>,
    /// The on-chain change time as epoch milliseconds.
    ///
    /// Selected as a BIGINT rather than the underlying `timestamp(6) with time
    /// zone`: `trino-rust-client` decodes timestamptz with a millisecond format
    /// string, which a microsecond column does not match.
    changed_at_ms: i64,
}

impl InventoryRow {
    /// Change-detection hash over every field the inventory actually carries.
    /// Drives `last_changed_at` in [`Gateway::insert_bulk`].
    fn compute_hash(
        location: Option<u64>,
        elevation: Option<u32>,
        is_full: Option<bool>,
    ) -> String {
        let mut hasher = DefaultHasher::new();
        hasher.write_u64(location.unwrap_or(0));
        hasher.write_u32(elevation.unwrap_or(0));
        hasher.write_u8(is_full.unwrap_or(false) as u8);
        hasher.finish().to_string()
    }

    /// Map a row onto a [`Gateway`], or `None` if it can't be decoded. A single bad
    /// row is logged and skipped rather than failing the tick.
    fn to_gateway(&self) -> Option<Gateway> {
        let address = match self.pub_key.parse::<PublicKeyBinary>() {
            Ok(address) => address,
            Err(err) => {
                tracing::warn!(
                    pub_key = %self.pub_key,
                    ?err,
                    "skipping inventory row with unparseable pub_key"
                );
                return None;
            }
        };

        // The column is an unprefixed lowercase H3 hex string, e.g. "8828308280fffff".
        // A value we can't decode degrades the gateway to unasserted rather than
        // dropping it: without a row the service answers not-found and the verifier
        // discards the hotspot's beacons entirely, which is far worse than serving it
        // with no location.
        let location = match self.asserted_hex.as_deref() {
            None => None,
            Some(hex) => match u64::from_str_radix(hex, 16) {
                Ok(location) => Some(location),
                Err(err) => {
                    tracing::warn!(
                        pub_key = %self.pub_key,
                        asserted_hex = %hex,
                        ?err,
                        "treating gateway as unasserted, unparseable asserted_hex"
                    );
                    None
                }
            },
        };

        let Some(changed_at) = DateTime::<Utc>::from_timestamp_millis(self.changed_at_ms) else {
            tracing::warn!(
                pub_key = %self.pub_key,
                changed_at_ms = self.changed_at_ms,
                "skipping inventory row with out-of-range timestamp"
            );
            return None;
        };

        let elevation = self.elevation.map(|e| e as u32);
        let is_full_hotspot = self.is_data_only.map(|data_only| !data_only);

        Some(Gateway {
            address,
            // Insert-only: preserved on conflict, so this is first-seen (see
            // `Gateway::insert_bulk`).
            created_at: changed_at,
            elevation,
            // Not carried by the chain pipeline; DEFAULT_GAIN is substituted when served.
            gain: None,
            hash: Self::compute_hash(location, elevation, is_full_hotspot),
            // Not carried by the chain pipeline, and not served.
            is_active: None,
            is_full_hotspot,
            // Recomputed by the upsert's CASE; see `Gateway::insert_bulk`.
            last_changed_at: Utc::now(),
            location,
            // Not carried by the chain pipeline, and not served.
            location_asserts: None,
            location_changed_at: location.map(|_| changed_at),
            refreshed_at: changed_at,
            updated_at: Utc::now(),
        })
    }
}

/// Fetch one keyset page of the inventory, ordered by `pub_key` and starting
/// strictly after `after`. An empty `after` starts from the beginning, since every
/// b58 pubkey sorts after the empty string.
pub async fn fetch_page(
    client: &trino_client::Client,
    table: &str,
    after: &str,
) -> anyhow::Result<Vec<InventoryRow>> {
    Ok(client.get_all(page_statement(table, after)).await?)
}

fn page_statement(table: &str, after: &str) -> trino_client::TypedStatement<InventoryRow> {
    trino_client::Statement::new(format!(
        r#"
            SELECT
                pub_key,
                asserted_hex,
                elevation,
                is_data_only,
                CAST(to_unixtime("timestamp") * 1000 AS BIGINT) AS changed_at_ms
            FROM {table}
            WHERE pub_key > :after
            ORDER BY pub_key
            LIMIT {PAGE_SIZE}
        "#
    ))
    .bind("after", after.to_string())
    .typed::<InventoryRow>()
}

/// The `pub_key` of the last row in a page, i.e. the cursor for the next one.
pub fn page_cursor(rows: &[InventoryRow]) -> Option<String> {
    rows.last().map(|row| row.pub_key.clone())
}

/// Map a page onto gateways, dropping rows that fail to decode.
pub fn page_to_gateways(rows: &[InventoryRow]) -> Vec<Gateway> {
    rows.iter().filter_map(InventoryRow::to_gateway).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use trino_client::SqlStatement;

    fn row(pub_key: &str, asserted_hex: Option<&str>) -> InventoryRow {
        InventoryRow {
            pub_key: pub_key.to_string(),
            asserted_hex: asserted_hex.map(str::to_string),
            elevation: Some(5),
            is_data_only: Some(false),
            changed_at_ms: 1_700_000_000_000,
        }
    }

    // A real b58 IoT hotspot key.
    const PUB_KEY: &str = "112NqN2WWMwtK29PMzRby62fDydBJfsCLkCAf392stdok48ovNT6";

    #[test]
    fn maps_an_asserted_row() {
        let gateway = row(PUB_KEY, Some("8828308280fffff")).to_gateway().unwrap();
        assert_eq!(gateway.location, Some(0x8828308280fffff));
        assert_eq!(gateway.elevation, Some(5));
        assert_eq!(gateway.is_full_hotspot, Some(true));
        // Not carried by the chain pipeline.
        assert_eq!(gateway.gain, None);
        assert_eq!(gateway.is_active, None);
        assert_eq!(gateway.location_asserts, None);
        // An asserted hotspot gets location_changed_at; refreshed_at is the
        // on-chain change time, not now.
        assert_eq!(gateway.refreshed_at, gateway.location_changed_at.unwrap());
        assert_eq!(gateway.created_at, gateway.refreshed_at);
    }

    #[test]
    fn unasserted_row_has_no_location_or_location_changed_at() {
        let gateway = row(PUB_KEY, None).to_gateway().unwrap();
        assert_eq!(gateway.location, None);
        assert_eq!(gateway.location_changed_at, None);
    }

    #[test]
    fn is_full_hotspot_inverts_is_data_only() {
        let mut r = row(PUB_KEY, None);
        r.is_data_only = Some(true);
        assert_eq!(r.to_gateway().unwrap().is_full_hotspot, Some(false));

        r.is_data_only = None;
        assert_eq!(r.to_gateway().unwrap().is_full_hotspot, None);
    }

    #[test]
    fn unparseable_pub_key_is_skipped_not_fatal() {
        // Without a pubkey there is no row to write at all.
        assert!(row("not-a-pubkey", None).to_gateway().is_none());
    }

    #[test]
    fn unparseable_asserted_hex_degrades_to_unasserted() {
        // The gateway is still served, just without a location — dropping it would
        // make the service answer not-found for a hotspot that exists.
        for hex in ["nothex", "", "0x8828308280fffff"] {
            let gateway = row(PUB_KEY, Some(hex))
                .to_gateway()
                .unwrap_or_else(|| panic!("{hex:?} should not drop the gateway"));
            assert_eq!(gateway.location, None);
            assert_eq!(gateway.location_changed_at, None);
            assert_eq!(gateway.elevation, Some(5));
        }
    }

    #[test]
    fn hash_changes_with_each_tracked_field() {
        let base = InventoryRow::compute_hash(Some(1), Some(2), Some(true));
        assert_ne!(
            base,
            InventoryRow::compute_hash(Some(9), Some(2), Some(true))
        );
        assert_ne!(
            base,
            InventoryRow::compute_hash(Some(1), Some(9), Some(true))
        );
        assert_ne!(
            base,
            InventoryRow::compute_hash(Some(1), Some(2), Some(false))
        );
        assert_eq!(
            base,
            InventoryRow::compute_hash(Some(1), Some(2), Some(true))
        );
    }

    #[test]
    fn page_statement_binds_cursor_and_targets_table() {
        let rendered = page_statement("cat.chain.iot_hotspot_inventory", "abc")
            .to_statement()
            .render()
            .unwrap();
        assert!(
            rendered.contains("cat.chain.iot_hotspot_inventory"),
            "{rendered}"
        );
        // Bound params render as positional placeholders (EXECUTE IMMEDIATE).
        assert!(rendered.contains("pub_key > ?"), "{rendered}");
        assert!(rendered.contains("ORDER BY pub_key"), "{rendered}");
    }

    #[test]
    fn page_cursor_is_the_last_pub_key() {
        assert_eq!(page_cursor(&[]), None);
        let rows = vec![row("aaa", None), row("zzz", None)];
        assert_eq!(page_cursor(&rows), Some("zzz".to_string()));
    }
}

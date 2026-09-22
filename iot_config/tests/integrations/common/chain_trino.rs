//! Trino fixtures for the on-chain tables iot-config reads.
//!
//! Production targets `network.chain.iot_hotspot_inventory` (Iceberg) and
//! `solana.public.sub_dao_epoch_infos` (a PostgreSQL-connector catalog over the
//! on-chain indexer). The test harness only serves Iceberg tables in a per-test
//! catalog, so both are recreated here and the callers are pointed at
//! `<catalog>.<namespace>.<table>` via their table/schema override seams.

use chrono::{DateTime, FixedOffset, Utc};
use helium_crypto::PublicKeyBinary;
use helium_iceberg::{FieldDefinition, IcebergTestHarness, PartitionDefinition, TableDefinition};
use serde::Serialize;

pub const CHAIN_NAMESPACE: &str = "chain";
pub const IOT_HOTSPOT_INVENTORY: &str = "iot_hotspot_inventory";

/// Mirrors production's `public` schema under the `solana` catalog.
pub const SOLANA_NAMESPACE: &str = "public";
pub const SUB_DAO_EPOCH_INFOS: &str = "sub_dao_epoch_infos";

/// The IoT sub-DAO's on-chain address.
pub const IOT_SUB_DAO: &str = "39Lw1RH6zt8AJvKn3BTxmUDofzduCM2J3kSaGDZ8L7Sk";
/// The mobile sub-DAO — seeded alongside IoT rows to prove the filter works.
pub const MOBILE_SUB_DAO: &str = "Gm9xDCJawDEKDrrQW6haw94gABaYzQwCq4ZQU8h8bd22";

// ── iot_hotspot_inventory ────────────────────────────────────────────────────

/// The columns glacon-rs writes to `chain.iot_hotspot_history`, which the dbt
/// inventory model carries through with `select *`.
#[derive(Serialize)]
pub struct InventoryRow {
    pub pub_key: String,
    pub asset: String,
    pub asserted_hex: Option<String>,
    pub elevation: Option<i32>,
    pub is_data_only: Option<bool>,
    pub timestamp: DateTime<FixedOffset>,
}

pub fn inventory_row(
    pub_key: &PublicKeyBinary,
    asserted_hex: Option<&str>,
    elevation: Option<i32>,
    is_data_only: Option<bool>,
    timestamp: DateTime<Utc>,
) -> InventoryRow {
    InventoryRow {
        pub_key: pub_key.to_string(),
        asset: format!("asset-{pub_key}"),
        asserted_hex: asserted_hex.map(str::to_string),
        elevation,
        is_data_only,
        timestamp: timestamp.fixed_offset(),
    }
}

/// A row whose `pub_key` is not a valid b58 public key, to exercise skip-not-fail.
pub fn invalid_inventory_row(pub_key: &str, timestamp: DateTime<Utc>) -> InventoryRow {
    InventoryRow {
        pub_key: pub_key.to_string(),
        asset: "asset-invalid".to_string(),
        asserted_hex: Some("8828308280fffff".to_string()),
        elevation: Some(5),
        is_data_only: Some(false),
        timestamp: timestamp.fixed_offset(),
    }
}

pub fn inventory_table() -> anyhow::Result<TableDefinition> {
    Ok(
        TableDefinition::builder(CHAIN_NAMESPACE, IOT_HOTSPOT_INVENTORY)
            .with_fields([
                FieldDefinition::required_string("pub_key"),
                FieldDefinition::required_string("asset"),
                // Nullable: a hotspot that never asserted carries NULL.
                FieldDefinition::optional_string("asserted_hex"),
                FieldDefinition::optional_int("elevation"),
                FieldDefinition::optional_boolean("is_data_only"),
                FieldDefinition::required_timestamptz("timestamp"),
            ])
            .with_partition(PartitionDefinition::day("timestamp", "timestamp_day"))
            .build()?,
    )
}

// ── sub_dao_epoch_infos ──────────────────────────────────────────────────────

/// The indexer's numeric columns do not surface as native bigints through the
/// `solana` catalog, so every field is a string — exactly what the resolver parses.
#[derive(Serialize)]
pub struct SubDaoEpochInfo {
    pub epoch: String,
    pub sub_dao: String,
    pub address: String,
    pub hnt_rewards_issued: String,
    pub delegation_rewards_issued: String,
    pub rewards_issued_at: String,
}

pub fn sub_dao_row(
    epoch: u64,
    sub_dao: &str,
    address: &str,
    hnt_rewards_issued: &str,
    delegation_rewards_issued: &str,
    rewards_issued_at: &str,
) -> SubDaoEpochInfo {
    SubDaoEpochInfo {
        epoch: epoch.to_string(),
        sub_dao: sub_dao.to_string(),
        address: address.to_string(),
        hnt_rewards_issued: hnt_rewards_issued.to_string(),
        delegation_rewards_issued: delegation_rewards_issued.to_string(),
        rewards_issued_at: rewards_issued_at.to_string(),
    }
}

pub fn sub_dao_epoch_infos_table() -> anyhow::Result<TableDefinition> {
    Ok(
        TableDefinition::builder(SOLANA_NAMESPACE, SUB_DAO_EPOCH_INFOS)
            .with_fields([
                FieldDefinition::required_string("epoch"),
                FieldDefinition::required_string("sub_dao"),
                FieldDefinition::required_string("address"),
                FieldDefinition::required_string("hnt_rewards_issued"),
                FieldDefinition::required_string("delegation_rewards_issued"),
                FieldDefinition::required_string("rewards_issued_at"),
            ])
            .with_partition(PartitionDefinition::identity("epoch"))
            .build()?,
    )
}

// ── harness helpers ──────────────────────────────────────────────────────────

pub async fn harness_with_inventory() -> anyhow::Result<IcebergTestHarness> {
    Ok(IcebergTestHarness::new_with_tables([inventory_table()?]).await?)
}

pub async fn harness_with_sub_dao() -> anyhow::Result<IcebergTestHarness> {
    Ok(IcebergTestHarness::new_with_tables([sub_dao_epoch_infos_table()?]).await?)
}

/// Fully-qualified name of the seeded inventory table, standing in for
/// `network.chain.iot_hotspot_inventory`.
pub fn inventory_table_name(h: &IcebergTestHarness) -> String {
    format!(
        "{}.{}.{}",
        h.catalog_name(),
        CHAIN_NAMESPACE,
        IOT_HOTSPOT_INVENTORY
    )
}

/// `catalog.schema` holding the seeded epoch table, standing in for `solana.public`.
pub fn solana_schema(h: &IcebergTestHarness) -> String {
    format!("{}.{}", h.catalog_name(), SOLANA_NAMESPACE)
}

pub async fn seed_inventory(
    h: &IcebergTestHarness,
    id: &str,
    rows: Vec<InventoryRow>,
) -> anyhow::Result<()> {
    h.get_table_writer_in::<InventoryRow>(CHAIN_NAMESPACE, IOT_HOTSPOT_INVENTORY)
        .await?
        .write_idempotent(id, rows)
        .await?;
    Ok(())
}

pub async fn seed_sub_dao(
    h: &IcebergTestHarness,
    id: &str,
    rows: Vec<SubDaoEpochInfo>,
) -> anyhow::Result<()> {
    h.get_table_writer_in::<SubDaoEpochInfo>(SOLANA_NAMESPACE, SUB_DAO_EPOCH_INFOS)
        .await?
        .write_idempotent(id, rows)
        .await?;
    Ok(())
}

pub async fn trino_client(h: &IcebergTestHarness) -> anyhow::Result<trino_client::Client> {
    Ok(trino_client::Client::from_client(h.owned_trino().await?))
}

use crate::common::{
    self,
    chain_trino::{
        harness_with_inventory, invalid_inventory_row, inventory_row, inventory_table_name,
        seed_inventory, trino_client,
    },
    make_keypair,
};
use chrono::{DateTime, Utc};
use helium_crypto::PublicKeyBinary;
use iot_config::gateway::{db::Gateway, tracker};
use sqlx::PgPool;

/// H3 cells, as the unprefixed lowercase hex strings the inventory stores.
const HEX_1: &str = "8c2681a3064d9ff";
const HEX_2: &str = "8c2681a3064dbff";

fn location(hex: &str) -> u64 {
    u64::from_str_radix(hex, 16).unwrap()
}

/// A millisecond-aligned timestamp `secs_ago` before now.
///
/// The inventory's `timestamp` is selected as epoch milliseconds, so anything finer
/// is truncated on the way through. Seeding aligned values keeps the assertions
/// exact instead of hiding the truncation behind a fuzzy comparison.
fn ts(secs_ago: i64) -> DateTime<Utc> {
    let millis = (Utc::now() - chrono::Duration::seconds(secs_ago)).timestamp_millis();
    DateTime::from_timestamp_millis(millis).unwrap()
}

/// Run one tracker tick against the seeded inventory.
async fn execute(
    pool: &PgPool,
    trino: &trino_client::Client,
    inventory_table: &str,
) -> anyhow::Result<()> {
    tracker::execute(pool, trino, inventory_table).await
}

#[sqlx::test]
async fn maps_inventory_rows_onto_gateways(pool: PgPool) -> anyhow::Result<()> {
    let pubkey: PublicKeyBinary = make_keypair().public_key().clone().into();
    let changed_at = ts(10);

    let h = harness_with_inventory().await?;
    let trino = trino_client(&h).await?;
    let table = inventory_table_name(&h);

    seed_inventory(
        &h,
        "initial",
        vec![inventory_row(
            &pubkey,
            Some(HEX_1),
            Some(1),
            Some(false),
            changed_at,
        )],
    )
    .await?;

    execute(&pool, &trino, &table).await?;

    let gateway = Gateway::get_by_address(&pool, &pubkey)
        .await?
        .expect("gateway not found");

    assert_eq!(gateway.location, Some(location(HEX_1)));
    assert_eq!(gateway.elevation, Some(1));
    // is_data_only = false means it is a full hotspot.
    assert_eq!(gateway.is_full_hotspot, Some(true));
    // Not carried by the chain pipeline.
    assert_eq!(gateway.gain, None);
    assert_eq!(gateway.is_active, None);
    assert_eq!(gateway.location_asserts, None);
    // refreshed_at is the on-chain change time, not the time of the run.
    assert_eq!(gateway.refreshed_at, common::nanos_trunc(changed_at));
    assert_eq!(gateway.created_at, common::nanos_trunc(changed_at));
    assert_eq!(
        gateway.location_changed_at,
        Some(common::nanos_trunc(changed_at))
    );

    Ok(())
}

#[sqlx::test]
async fn location_change_advances_changed_at_and_keeps_created_at(
    pool: PgPool,
) -> anyhow::Result<()> {
    let pubkey: PublicKeyBinary = make_keypair().public_key().clone().into();
    let first_seen = ts(60);

    let h = harness_with_inventory().await?;
    let trino = trino_client(&h).await?;
    let table = inventory_table_name(&h);

    seed_inventory(
        &h,
        "initial",
        vec![inventory_row(
            &pubkey,
            Some(HEX_1),
            Some(1),
            Some(false),
            first_seen,
        )],
    )
    .await?;
    execute(&pool, &trino, &table).await?;

    // The dbt model keeps one row per pub_key; simulate the refresh by pointing the
    // tracker at a table holding only the newer row.
    let h2 = harness_with_inventory().await?;
    let trino2 = trino_client(&h2).await?;
    let table2 = inventory_table_name(&h2);
    let moved_at = ts(0);
    seed_inventory(
        &h2,
        "moved",
        vec![inventory_row(
            &pubkey,
            Some(HEX_2),
            Some(10),
            Some(true),
            moved_at,
        )],
    )
    .await?;
    execute(&pool, &trino2, &table2).await?;

    let gateway = Gateway::get_by_address(&pool, &pubkey)
        .await?
        .expect("gateway not found");

    assert_eq!(gateway.location, Some(location(HEX_2)));
    assert_eq!(gateway.elevation, Some(10));
    assert_eq!(gateway.is_full_hotspot, Some(false));
    assert_eq!(gateway.refreshed_at, common::nanos_trunc(moved_at));
    // Both advance because location and hash changed.
    assert_eq!(gateway.last_changed_at, common::nanos_trunc(moved_at));
    assert_eq!(
        gateway.location_changed_at,
        Some(common::nanos_trunc(moved_at))
    );
    // created_at is insert-only: still the first sighting.
    assert_eq!(gateway.created_at, common::nanos_trunc(first_seen));

    Ok(())
}

#[sqlx::test]
async fn unasserted_hotspot_has_no_location(pool: PgPool) -> anyhow::Result<()> {
    let pubkey: PublicKeyBinary = make_keypair().public_key().clone().into();
    let changed_at = ts(10);

    let h = harness_with_inventory().await?;
    let trino = trino_client(&h).await?;
    let table = inventory_table_name(&h);

    seed_inventory(
        &h,
        "unasserted",
        vec![inventory_row(&pubkey, None, None, Some(false), changed_at)],
    )
    .await?;
    execute(&pool, &trino, &table).await?;

    let gateway = Gateway::get_by_address(&pool, &pubkey)
        .await?
        .expect("gateway not found");

    assert_eq!(gateway.location, None);
    assert_eq!(gateway.location_changed_at, None);
    assert_eq!(gateway.elevation, None);

    Ok(())
}

#[sqlx::test]
async fn unparseable_pub_key_is_skipped_not_fatal(pool: PgPool) -> anyhow::Result<()> {
    let pubkey: PublicKeyBinary = make_keypair().public_key().clone().into();
    let changed_at = ts(10);

    let h = harness_with_inventory().await?;
    let trino = trino_client(&h).await?;
    let table = inventory_table_name(&h);

    seed_inventory(
        &h,
        "mixed",
        vec![
            inventory_row(&pubkey, Some(HEX_1), Some(1), Some(false), changed_at),
            invalid_inventory_row("not-a-valid-b58-pubkey", changed_at),
        ],
    )
    .await?;

    execute(&pool, &trino, &table).await?;

    assert!(Gateway::get_by_address(&pool, &pubkey).await?.is_some());

    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM gateways")
        .fetch_one(&pool)
        .await?;
    assert_eq!(count, 1, "only the valid gateway should be inserted");

    Ok(())
}

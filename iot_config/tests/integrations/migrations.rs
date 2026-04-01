use crate::common::partial_migrator::PartialMigrator;
use sqlx::PgPool;

#[sqlx::test(migrations = false)]
async fn multibuy_migration(pool: PgPool) -> anyhow::Result<()> {
    let partial_migrator = PartialMigrator::new(pool.clone(), vec![20260331000000]).await?;

    partial_migrator.run_partial().await?;

    // Insert an org and a route before the multibuy migration
    sqlx::query(
        r#"
        INSERT INTO organizations (oui, owner_pubkey, payer_pubkey)
        VALUES (1, 'owner', 'payer')
        "#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO routes (oui, net_id, max_copies, server_host, server_port, server_protocol_opts)
        VALUES (1, 1, 1, 'localhost', 8080, '{"protocol": "packet_router"}')
        "#,
    )
    .execute(&pool)
    .await?;

    // Apply the multibuy migration
    partial_migrator.run_skipped().await?;

    // Verify the route still exists and multi_buy is NULL
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM routes")
        .fetch_one(&pool)
        .await?;
    assert_eq!(count, 1, "route should still exist after migration");

    let multi_buy: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT multi_buy FROM routes LIMIT 1")
            .fetch_one(&pool)
            .await?;
    assert!(
        multi_buy.is_none(),
        "multi_buy should be NULL for existing routes"
    );

    Ok(())
}

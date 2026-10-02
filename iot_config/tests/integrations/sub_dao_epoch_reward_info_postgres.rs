//! End-to-end test of the sub-DAO epoch reward-info resolution against the real
//! Postgres-backed table it reads in production.
//!
//! Production reads `solana.public.sub_dao_epoch_infos` through a Trino
//! postgresql-connector catalog. Unlike the iceberg-backed
//! `sub_dao_epoch_reward_info` test, here the table is created in the
//! `#[sqlx::test]` database with its real Postgres column types, and a dynamic
//! Trino catalog is registered against that database so the query runs through
//! the same connector as production.

use crate::common::chain_trino::{IOT_SUB_DAO, MOBILE_SUB_DAO};
use helium_iceberg::HarnessConfig;
use helium_proto::services::sub_dao::SubDaoEpochRewardInfo;
use iot_config::sub_dao_epoch_reward_info::trino::get_info;
use sqlx::PgPool;

/// Postgres as seen from inside the Trino container (the `postgres` service in
/// both docker-compose and CI).
const TRINO_POSTGRES_HOST: &str = "postgres:5432";

#[sqlx::test(migrations = false)]
async fn resolves_the_iot_row_from_postgres(pool: PgPool) -> anyhow::Result<()> {
    create_table(&pool).await?;
    // Real prod values for epoch 20654. Both sub-DAOs report for the same epoch;
    // only the IoT row may be returned.
    seed(
        &pool,
        &[
            (
                "2oLR5eYkFdvvRoaQ1L3V1cDjCeFNmiQE67GkGkN5ZW9N",
                20654,
                IOT_SUB_DAO,
                301_412_090_426,
                19_239_069_601,
                1_784_592_033,
            ),
            (
                "aKtGx8Hf4FMDLm3Xbp4UGGP8UFRw4Azo71VscGcRum5",
                20654,
                MOBILE_SUB_DAO,
                2_599_729_243_320,
                165_940_164_467,
                1_784_592_034,
            ),
        ],
    )
    .await?;

    let (trino, catalog) = create_catalog(&pool).await?;

    let info = get_info(&trino, &format!("{catalog}.public"), 20654, IOT_SUB_DAO)
        .await?
        .expect("expected reward info for a closed epoch");

    let proto: SubDaoEpochRewardInfo = info.into();
    assert_eq!(
        proto,
        SubDaoEpochRewardInfo {
            epoch: 20654,
            epoch_address: "2oLR5eYkFdvvRoaQ1L3V1cDjCeFNmiQE67GkGkN5ZW9N".into(),
            sub_dao_address: IOT_SUB_DAO.into(),
            hnt_rewards_issued: 301_412_090_426,
            delegation_rewards_issued: 19_239_069_601,
            rewards_issued_at: 1_784_592_033,
        }
    );

    drop_catalog(&trino, &catalog).await
}

/// Values and an epoch with trailing zeros are the ones Trino >= 480 would
/// render in exponent form (`1784246400` -> `1.7842464E+9`) without the
/// `DECIMAL(38, 0)` casts, failing the parse and the epoch match.
#[sqlx::test(migrations = false)]
async fn trailing_zeros_resolve_as_plain_digits(pool: PgPool) -> anyhow::Result<()> {
    create_table(&pool).await?;
    seed(
        &pool,
        &[(
            "2oLR5eYkFdvvRoaQ1L3V1cDjCeFNmiQE67GkGkN5ZW9N",
            20650,
            IOT_SUB_DAO,
            301_412_090_000,
            19_239_000_000,
            1_784_246_400,
        )],
    )
    .await?;

    let (trino, catalog) = create_catalog(&pool).await?;

    let info = get_info(&trino, &format!("{catalog}.public"), 20650, IOT_SUB_DAO)
        .await?
        .expect("expected reward info for an epoch ending in 0");

    let proto: SubDaoEpochRewardInfo = info.into();
    assert_eq!(proto.epoch, 20650);
    assert_eq!(proto.hnt_rewards_issued, 301_412_090_000);
    assert_eq!(proto.delegation_rewards_issued, 19_239_000_000);
    assert_eq!(proto.rewards_issued_at, 1_784_246_400);

    drop_catalog(&trino, &catalog).await
}

/// The production indexer DDL for `sub_dao_epoch_infos`.
async fn create_table(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE public.sub_dao_epoch_infos (
            address character varying(255) NOT NULL,
            epoch numeric,
            sub_dao character varying(255),
            dc_burned numeric,
            vehnt_at_epoch_start numeric,
            vehnt_in_closing_positions numeric,
            fall_rates_from_closing_positions numeric,
            delegation_rewards_issued numeric,
            utility_score numeric,
            rewards_issued_at numeric,
            bump_seed integer,
            initialized boolean,
            dc_onboarding_fees_paid numeric,
            refreshed_at timestamp with time zone,
            created_at timestamp with time zone NOT NULL,
            hnt_rewards_issued numeric,
            previous_percentage numeric
        )
        "#,
    )
    .execute(pool)
    .await?;

    Ok(())
}

/// Rows of `(address, epoch, sub_dao, hnt_rewards_issued,
/// delegation_rewards_issued, rewards_issued_at)`.
async fn seed(pool: &PgPool, rows: &[(&str, i64, &str, i64, i64, i64)]) -> anyhow::Result<()> {
    for (address, epoch, sub_dao, hnt, delegation, issued_at) in rows {
        sqlx::query(
            r#"
            INSERT INTO public.sub_dao_epoch_infos (
                address, epoch, sub_dao, hnt_rewards_issued,
                delegation_rewards_issued, rewards_issued_at, created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, now())
            "#,
        )
        .bind(address)
        .bind(epoch)
        .bind(sub_dao)
        .bind(hnt)
        .bind(delegation)
        .bind(issued_at)
        .execute(pool)
        .await?;
    }

    Ok(())
}

/// Register a Trino postgresql catalog against this test's database: plain
/// connection properties plus
/// `unsupported-type-handling = CONVERT_TO_VARCHAR` so the unbounded `numeric`
/// columns are exposed (as varchar on Trino <= 479) instead of silently
/// dropped. Returns the client and the catalog name.
async fn create_catalog(pool: &PgPool) -> anyhow::Result<(trino_client::Client, String)> {
    let trino =
        trino_client::Client::from_client(HarnessConfig::default().trino_client_builder().build()?);

    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(pool)
        .await?;
    let catalog = format!("pg{}", database.to_lowercase());

    // The name is stable across runs (sqlx derives the database name from the
    // test path), so clear a catalog left behind by a failed run.
    trino
        .execute_raw(format!(r#"DROP CATALOG IF EXISTS "{catalog}""#))
        .await?;

    trino
        .execute_raw(format!(
            r#"
            CREATE CATALOG "{catalog}" USING postgresql WITH (
                "connection-url" = 'jdbc:postgresql://{TRINO_POSTGRES_HOST}/{database}',
                "connection-user" = 'postgres',
                "connection-password" = 'postgres',
                "unsupported-type-handling" = 'CONVERT_TO_VARCHAR'
            )
            "#
        ))
        .await?;

    Ok((trino, catalog))
}

/// Only reached on success: a failed run keeps its catalog for inspection, like
/// sqlx keeps its database, and `create_catalog` clears it next run.
async fn drop_catalog(trino: &trino_client::Client, catalog: &str) -> anyhow::Result<()> {
    trino
        .execute_raw(format!(r#"DROP CATALOG "{catalog}""#))
        .await?;
    Ok(())
}

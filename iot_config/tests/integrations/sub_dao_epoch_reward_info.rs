//! End-to-end test of the sub-DAO epoch reward-info resolution against a real
//! Trino, seeding the on-chain epoch table the service reads from.
//!
//! Production targets `solana.public.sub_dao_epoch_infos` (a PostgreSQL-connector
//! catalog). The harness only serves Iceberg tables in a per-test catalog, so the
//! table is recreated with all columns as varchar — matching how the indexer's
//! numerics surface — and the query is pointed at `<catalog>.public`.

use crate::common::chain_trino::{
    harness_with_sub_dao, seed_sub_dao, solana_schema, sub_dao_row, trino_client, IOT_SUB_DAO,
    MOBILE_SUB_DAO,
};
use iot_config::sub_dao_epoch_reward_info::trino::get_info;

// Real prod values for epoch 20654.
const EPOCH: u64 = 20654;
const IOT_EPOCH_ADDRESS: &str = "2oLR5eYkFdvvRoaQ1L3V1cDjCeFNmiQE67GkGkN5ZW9N";
const IOT_HNT_REWARDS: &str = "301412090426";
const IOT_DELEGATION_REWARDS: &str = "19239069601";
const IOT_REWARDS_ISSUED_AT: &str = "1784592033";

#[tokio::test]
async fn resolves_the_iot_row_and_ignores_other_sub_daos() -> anyhow::Result<()> {
    let h = harness_with_sub_dao().await?;
    let client = trino_client(&h).await?;

    // Both sub-DAOs report for the same epoch; only the IoT row may be returned.
    seed_sub_dao(
        &h,
        "epoch",
        vec![
            sub_dao_row(
                EPOCH,
                IOT_SUB_DAO,
                IOT_EPOCH_ADDRESS,
                IOT_HNT_REWARDS,
                IOT_DELEGATION_REWARDS,
                IOT_REWARDS_ISSUED_AT,
            ),
            sub_dao_row(
                EPOCH,
                MOBILE_SUB_DAO,
                "aKtGx8Hf4FMDLm3Xbp4UGGP8UFRw4Azo71VscGcRum5",
                "2599729243320",
                "165940164467",
                "1784592034",
            ),
        ],
    )
    .await?;

    let info = get_info(&client, &solana_schema(&h), EPOCH, IOT_SUB_DAO)
        .await?
        .expect("expected reward info for a closed epoch");

    let proto: helium_proto::services::sub_dao::SubDaoEpochRewardInfo = info.into();
    assert_eq!(proto.epoch, EPOCH);
    assert_eq!(proto.epoch_address, IOT_EPOCH_ADDRESS);
    assert_eq!(proto.sub_dao_address, IOT_SUB_DAO);
    assert_eq!(proto.hnt_rewards_issued, 301_412_090_426);
    assert_eq!(proto.delegation_rewards_issued, 19_239_069_601);
    // Encoded back to the epoch seconds it was seeded with.
    assert_eq!(proto.rewards_issued_at, 1_784_592_033);

    Ok(())
}

#[tokio::test]
async fn unclosed_epoch_reads_as_not_ready() -> anyhow::Result<()> {
    let h = harness_with_sub_dao().await?;
    let client = trino_client(&h).await?;

    // The row exists but the chain hasn't issued yet.
    seed_sub_dao(
        &h,
        "unclosed",
        vec![sub_dao_row(
            EPOCH,
            IOT_SUB_DAO,
            IOT_EPOCH_ADDRESS,
            "0",
            "0",
            "0",
        )],
    )
    .await?;

    assert!(get_info(&client, &solana_schema(&h), EPOCH, IOT_SUB_DAO)
        .await?
        .is_none());

    Ok(())
}

#[tokio::test]
async fn one_zero_field_still_resolves() -> anyhow::Result<()> {
    let h = harness_with_sub_dao().await?;
    let client = trino_client(&h).await?;

    // Only both-zero means "not issued yet"; a single zero is a real value.
    seed_sub_dao(
        &h,
        "partial",
        vec![sub_dao_row(
            EPOCH,
            IOT_SUB_DAO,
            IOT_EPOCH_ADDRESS,
            IOT_HNT_REWARDS,
            "0",
            IOT_REWARDS_ISSUED_AT,
        )],
    )
    .await?;

    let info = get_info(&client, &solana_schema(&h), EPOCH, IOT_SUB_DAO)
        .await?
        .expect("a single zero field is still a resolved epoch");
    let proto: helium_proto::services::sub_dao::SubDaoEpochRewardInfo = info.into();
    assert_eq!(proto.delegation_rewards_issued, 0);

    Ok(())
}

#[tokio::test]
async fn missing_epoch_reads_as_not_ready() -> anyhow::Result<()> {
    let h = harness_with_sub_dao().await?;
    let client = trino_client(&h).await?;

    seed_sub_dao(
        &h,
        "other-epoch",
        vec![sub_dao_row(
            EPOCH,
            IOT_SUB_DAO,
            IOT_EPOCH_ADDRESS,
            IOT_HNT_REWARDS,
            IOT_DELEGATION_REWARDS,
            IOT_REWARDS_ISSUED_AT,
        )],
    )
    .await?;

    assert!(
        get_info(&client, &solana_schema(&h), EPOCH + 1, IOT_SUB_DAO)
            .await?
            .is_none()
    );

    Ok(())
}

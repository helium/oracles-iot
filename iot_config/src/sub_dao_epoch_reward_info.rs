use crate::EpochInfo;
use chrono::{DateTime, Utc};
use file_store::traits::{TimestampDecode, TimestampDecodeError, TimestampEncode};
use helium_proto::services::sub_dao::SubDaoEpochRewardInfo as SubDaoEpochRewardInfoProto;
use rust_decimal::prelude::*;
use std::ops::Range;

#[derive(Clone, Debug)]
pub struct EpochRewardInfo {
    pub epoch_day: u64,
    pub epoch_address: String,
    pub sub_dao_address: String,
    pub epoch_period: Range<DateTime<Utc>>,
    pub epoch_emissions: Decimal,
    pub rewards_issued_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct RawSubDaoEpochRewardInfo {
    epoch: u64,
    epoch_address: String,
    sub_dao_address: String,
    hnt_rewards_issued: u64,
    delegation_rewards_issued: u64,
    rewards_issued_at: DateTime<Utc>,
}

#[derive(thiserror::Error, Debug)]
pub enum SubDaoRewardInfoParseError {
    #[error("invalid timestamp: {0}")]
    Timestamp(#[from] TimestampDecodeError),
}

impl From<RawSubDaoEpochRewardInfo> for SubDaoEpochRewardInfoProto {
    fn from(info: RawSubDaoEpochRewardInfo) -> Self {
        Self {
            epoch: info.epoch,
            epoch_address: info.epoch_address,
            sub_dao_address: info.sub_dao_address,
            hnt_rewards_issued: info.hnt_rewards_issued,
            delegation_rewards_issued: info.delegation_rewards_issued,
            rewards_issued_at: info.rewards_issued_at.encode_timestamp(),
        }
    }
}

impl TryFrom<SubDaoEpochRewardInfoProto> for EpochRewardInfo {
    type Error = SubDaoRewardInfoParseError;

    fn try_from(info: SubDaoEpochRewardInfoProto) -> Result<Self, Self::Error> {
        let epoch_period: EpochInfo = info.epoch.into();
        let epoch_rewards = Decimal::from(info.hnt_rewards_issued + info.delegation_rewards_issued);

        Ok(Self {
            epoch_day: info.epoch,
            epoch_address: info.epoch_address,
            sub_dao_address: info.sub_dao_address,
            epoch_period: epoch_period.period,
            epoch_emissions: epoch_rewards,
            rewards_issued_at: info.rewards_issued_at.to_timestamp()?,
        })
    }
}

/// Reads the sub-DAO epoch reward info from the Solana on-chain indexer via Trino,
/// replacing a direct Postgres connection to the same database.
pub mod trino {
    use crate::sub_dao_epoch_reward_info::RawSubDaoEpochRewardInfo;
    use anyhow::Context;
    use chrono::{DateTime, Utc};
    use trino_client::TrinoFromRow;

    /// Catalog + schema holding the Solana on-chain indexer tables in production.
    /// The `solana` catalog is a Trino PostgreSQL connector over the indexer DB.
    /// Exposed so integration tests can point the query at seeded fixtures in
    /// another catalog.
    pub const SOLANA_SCHEMA: &str = "solana.public";

    /// One `sub_dao_epoch_infos` row. The indexer's numeric columns do not surface
    /// as native bigints through this catalog, so the query `CAST`s every one to
    /// varchar and they are parsed here. Field names match the `SELECT ... AS`
    /// aliases.
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, TrinoFromRow)]
    struct EpochRow {
        epoch_address: String,
        hnt_rewards_issued: String,
        delegation_rewards_issued: String,
        rewards_issued_at: String,
    }

    /// The epoch's reward info, or `None` when the chain hasn't issued for it yet —
    /// either the row isn't indexed, or it is present with both reward fields still
    /// zero. The caller waits and retries.
    pub async fn get_info(
        client: &trino_client::Client,
        schema: &str,
        epoch: u64,
        sub_dao: &str,
    ) -> anyhow::Result<Option<RawSubDaoEpochRewardInfo>> {
        let Some(row) = client
            .get_all(epoch_statement(schema, epoch, sub_dao))
            .await?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };

        let hnt_rewards_issued: u64 = row
            .hnt_rewards_issued
            .parse()
            .context("parsing sub_dao_epoch_infos.hnt_rewards_issued")?;
        let delegation_rewards_issued: u64 = row
            .delegation_rewards_issued
            .parse()
            .context("parsing sub_dao_epoch_infos.delegation_rewards_issued")?;

        // Both zero => the epoch hasn't been closed / had rewards issued yet.
        if hnt_rewards_issued == 0 && delegation_rewards_issued == 0 {
            return Ok(None);
        }

        let rewards_issued_at_secs: i64 = row
            .rewards_issued_at
            .parse()
            .context("parsing sub_dao_epoch_infos.rewards_issued_at")?;
        let rewards_issued_at = DateTime::<Utc>::from_timestamp(rewards_issued_at_secs, 0)
            .context("sub_dao_epoch_infos.rewards_issued_at out of range")?;

        Ok(Some(RawSubDaoEpochRewardInfo {
            epoch,
            epoch_address: row.epoch_address,
            sub_dao_address: sub_dao.to_string(),
            hnt_rewards_issued,
            delegation_rewards_issued,
            rewards_issued_at,
        }))
    }

    /// `epoch` is a varchar column, so it is bound as its decimal string. Qualifying
    /// the table with `schema` (catalog.schema) makes the reference independent of
    /// the client's default catalog.
    fn epoch_statement(
        schema: &str,
        epoch: u64,
        sub_dao: &str,
    ) -> trino_client::TypedStatement<EpochRow> {
        trino_client::Statement::new(format!(
            "
            SELECT
                address                                    AS epoch_address,
                CAST(hnt_rewards_issued AS VARCHAR)        AS hnt_rewards_issued,
                CAST(delegation_rewards_issued AS VARCHAR) AS delegation_rewards_issued,
                CAST(rewards_issued_at AS VARCHAR)         AS rewards_issued_at
            FROM {schema}.sub_dao_epoch_infos
            WHERE epoch = :epoch AND sub_dao = :sub_dao
            "
        ))
        .bind("epoch", epoch.to_string())
        .bind("sub_dao", sub_dao.to_string())
        .typed::<EpochRow>()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use trino_client::SqlStatement;

        #[test]
        fn statement_targets_the_solana_table_and_binds_both_keys() {
            let rendered = epoch_statement(SOLANA_SCHEMA, 20654, "39Lw1RH6zt8")
                .to_statement()
                .render()
                .unwrap();
            assert!(
                rendered.contains("solana.public.sub_dao_epoch_infos"),
                "{rendered}"
            );
            // Bound params render as positional placeholders (EXECUTE IMMEDIATE).
            assert!(rendered.contains("epoch = ?"), "{rendered}");
            assert!(rendered.contains("sub_dao = ?"), "{rendered}");
        }
    }
}

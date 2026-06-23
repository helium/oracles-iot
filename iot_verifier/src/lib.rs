extern crate tls_init;

pub mod backfill;
pub mod cli;
pub mod gateway_cache;
pub mod gateway_updater;
pub mod iceberg;
pub mod meta;
pub mod packet_loader;
pub mod region_cache;
pub mod reward_share;
pub mod rewarder;
mod settings;
pub mod telemetry;

use rust_decimal::Decimal;
pub use settings::Settings;
use solana::SolPubkey;

#[derive(Clone, Debug)]
pub struct PriceInfo {
    pub price_in_bones: u64,
    pub price_per_token: Decimal,
    pub price_per_bone: Decimal,
    pub decimals: u8,
}

impl PriceInfo {
    pub fn new(price_in_bones: u64, decimals: u8) -> Self {
        let price_per_token =
            Decimal::from(price_in_bones) / Decimal::from(10_u64.pow(decimals as u32));
        let price_per_bone = price_per_token / Decimal::from(10_u64.pow(decimals as u32));
        Self {
            price_in_bones,
            price_per_token,
            price_per_bone,
            decimals,
        }
    }
}

pub fn resolve_subdao_pubkey() -> SolPubkey {
    solana::SubDao::Iot.key()
}

#[cfg(test)]
tls_init::include_tls_tests!();

use config::{Config, Environment, File};
use humantime_serde::re::humantime;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FileStoreClients {
    /// Cache location for generated verified reports
    pub cache: PathBuf,

    /// Where does verifier write all it's output
    pub output: file_store::BucketSettings,

    /// HPR packet report bucket
    pub packet_input: file_store::BucketSettings,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Settings {
    /// RUST_LOG compatible settings string. Default to
    /// "iot_verifier=debug,poc_store=info"
    #[serde(default = "default_log")]
    pub log: String,
    #[serde(default)]
    pub custom_tracing: custom_tracing::Settings,

    pub file_store_clients: FileStoreClients,

    pub database: db_store::Settings,
    pub iot_config_client: iot_config::client::Settings,

    #[serde(default)]
    pub metrics: poc_metrics::Settings,

    pub price_tracker: price_tracker::Settings,

    /// Reward period in hours
    #[serde(with = "humantime_serde", default = "default_reward_period")]
    pub reward_period: Duration,

    /// Reward calculation offset in minutes, rewards will be calculated at the end
    /// of the reward_period + reward_period_offset
    #[serde(with = "humantime_serde", default = "default_reward_period_offset")]
    pub reward_period_offset: Duration,

    /// max window age for the packet loader
    #[serde(
        with = "humantime_serde",
        default = "default_loader_window_max_lookback_age"
    )]
    pub loader_window_max_lookback_age: Duration,

    /// File store poll interval for incoming packets
    #[serde(with = "humantime_serde", default = "default_packet_interval")]
    pub packet_interval: Duration,

    /// interval at which cached gateways are refreshed from iot config
    #[serde(with = "humantime_serde", default = "default_gateway_refresh_interval")]
    pub gateway_refresh_interval: Duration,

    /// Iceberg connection settings. When present, the rewarder mirrors every
    /// reward share it emits into the configured Iceberg tables in addition
    /// to the existing S3 file sink.
    #[serde(default)]
    pub iceberg_settings: Option<helium_iceberg::Settings>,
}

fn default_gateway_refresh_interval() -> Duration {
    humantime::parse_duration("30 minutes").unwrap()
}

fn default_loader_window_max_lookback_age() -> Duration {
    humantime::parse_duration("60 minutes").unwrap()
}

fn default_reward_period() -> Duration {
    humantime::parse_duration("24 hours").unwrap()
}

fn default_reward_period_offset() -> Duration {
    humantime::parse_duration("30 minutes").unwrap()
}

fn default_packet_interval() -> Duration {
    humantime::parse_duration("15 minutes").unwrap()
}

fn default_log() -> String {
    "iot_verifier=debug".to_string()
}

impl Settings {
    /// Load Settings from a given path. Settings are loaded from a given
    /// optional path and can be overridden with environment variables.
    ///
    /// Environment overrides have the same name as the entries in the settings
    /// file in uppercase and prefixed with "VERIFY_". For example
    /// "VERIFY_DATABASE_URL" will override the data base url.
    pub fn new<P: AsRef<Path>>(path: Option<P>) -> Result<Self, config::ConfigError> {
        let mut builder = Config::builder();

        if let Some(file) = path {
            builder = builder
                .add_source(File::with_name(&file.as_ref().to_string_lossy()).required(false));
        }
        builder
            .add_source(Environment::with_prefix("VERIFY").separator("__"))
            .build()
            .and_then(|config| config.try_deserialize())
    }

    pub fn as_json_pretty(&self) -> String {
        fn format_duration(d: Duration) -> String {
            humantime::format_duration(d).to_string()
        }

        serde_json::to_string_pretty(&serde_json::json!({
            "log": self.log,
            "custom_tracing": self.custom_tracing,
            "file_store_clients": self.file_store_clients,
            "database": self.database,
            "iot_config_client": {
                "url": self.iot_config_client.url.to_string(),
                "signing_keypair_pubkey": self.iot_config_client.signing_keypair.public_key().to_string(),
                "config_pubkey": self.iot_config_client.config_pubkey
            },
            "metrics": self.metrics,
            "price_tracker": self.price_tracker,
            "rewarding": {
                "period": format_duration(self.reward_period),
                "offset": format_duration(self.reward_period_offset)
            },
            "loader_window_max_lookback_age": format_duration(self.loader_window_max_lookback_age),
            "packet_interval": format_duration(self.packet_interval),
            "gateway_refresh_interval": format_duration(self.gateway_refresh_interval),
        }))
        .expect("printing settings")
    }
}

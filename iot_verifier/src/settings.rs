use config::{Config, Environment, File};
use humantime_serde::re::humantime;
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Duration};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FileStoreClients {
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

    /// Every bucket the verifier writes reward shares and manifests to, and
    /// the directory they stage under. Replaces `file_store_clients.cache`
    /// and `file_store_clients.output`.
    pub file_upload: file_store::file_upload::Settings,

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
            "file_upload": self.file_upload,
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

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use helium_crypto::{KeyTag, Keypair};
    use std::io::Write;

    const R2_KEY_ID: &str = "AKIAsupersecretkeyid";
    const R2_SECRET: &str = "r2-secret-access-key-value";
    const DB_PASSWORD: &str = "hunter2";

    /// `as_json_pretty` is how the server logs its configuration at boot, so an
    /// upload bucket's credentials must not survive the round trip.
    ///
    /// Worth pinning rather than trusting by inspection: `BucketSettings` carries
    /// its credentials in a `#[serde(flatten)]`ed inner struct, so the
    /// `skip_serializing` that redacts them sits one level below the field this
    /// settings struct names. The R2 mirror's key pair belongs in the
    /// environment, which is exactly the case where a leak would land in the logs.
    #[test]
    fn serialized_settings_carry_no_upload_credentials() {
        let keypair = Keypair::generate(KeyTag::default(), &mut rand::rngs::OsRng);
        let encoded = base64::engine::general_purpose::STANDARD.encode(keypair.to_vec());

        let mut file = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .expect("temp settings file");
        write!(
            file,
            r#"
            [file_store_clients.packet_input]
            bucket = "mainnet-iot-packet-ingest"

            [file_upload]
            root = "/var/data/iot-verified"

            [file_upload.buckets.s3]
            bucket = "mainnet-iot-verified"
            region = "us-west-2"

            [file_upload.buckets.r2]
            bucket = "mainnet-iot-verified-mirror"
            endpoint = "https://account.r2.cloudflarestorage.com"
            region = "auto"
            access_key_id = "{R2_KEY_ID}"
            secret_access_key = "{R2_SECRET}"

            [database]
            url = "postgres://postgres:{DB_PASSWORD}@127.0.0.1:5432/iot_verifier"

            [iot_config_client]
            url = "http://127.0.0.1:8080"
            signing_keypair = "{encoded}"
            config_pubkey = "137oJzq1qZpSbzHawaysTGGsRCYTXG1MiTMQNxYSsQJp4YMDdN8"

            [price_tracker]
            price_duration_minutes = 60

            [price_tracker.bucket]
            bucket = "mainnet-price"

            [metrics]
            endpoint = "127.0.0.1:19001"
            "#
        )
        .expect("write settings");

        let settings = Settings::new(Some(file.path())).expect("load settings");

        // Sanity first: the secrets really are in the loaded struct. Without this a
        // fixture that silently failed to set them would make the assertions below
        // pass while proving nothing.
        let r2 = &settings.file_upload.buckets["r2"];
        assert_eq!(r2.settings.access_key_id.as_deref(), Some(R2_KEY_ID));
        assert_eq!(r2.settings.secret_access_key.as_deref(), Some(R2_SECRET));
        assert!(
            settings
                .database
                .url
                .as_deref()
                .is_some_and(|url| url.contains(DB_PASSWORD)),
            "fixture did not load the database password"
        );

        let json = settings.as_json_pretty();

        assert!(
            !json.contains(R2_KEY_ID),
            "r2 access key id leaked into settings log:\n{json}"
        );
        assert!(
            !json.contains(R2_SECRET),
            "r2 secret access key leaked into settings log:\n{json}"
        );
        assert!(
            !json.contains(DB_PASSWORD),
            "database password leaked into settings log:\n{json}"
        );
        // ...while the non-secret configuration is still there to be useful.
        assert!(json.contains("mainnet-iot-verified-mirror"), "{json}");
        assert!(json.contains("/var/data/iot-verified"), "{json}");
    }
}

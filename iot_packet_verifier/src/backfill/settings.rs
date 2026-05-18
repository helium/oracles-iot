use config::{Config, Environment, File};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Clone)]
pub struct Settings {
    /// RUST_LOG compatible settings string. Default to
    /// "iot_packet_verifier=debug"
    #[serde(default = "default_log")]
    pub log: String,
    #[serde(default)]
    pub custom_tracing: custom_tracing::Settings,

    /// Bucket holding the `iot_valid_packet` files written by the daemon.
    pub ingest_bucket: file_store::BucketSettings,

    pub database: db_store::Settings,

    /// Folder for local cache of backfill state, including the iceberg
    /// `BatchedWriter` spool. Persisted between runs so any pending
    /// (un-committed) records survive a restart and are replayed on
    /// startup.
    #[serde(default = "default_cache")]
    pub cache: PathBuf,

    /// Iceberg connection settings (REST catalog, S3, auth). Required by the
    /// `backfill-valid-packets` subcommand.
    #[serde(default)]
    pub iceberg_settings: Option<helium_iceberg::Settings>,
}

fn default_log() -> String {
    "iot_packet_verifier=debug".to_string()
}

fn default_cache() -> PathBuf {
    PathBuf::from("/var/data/iot-packet-verifier-backfill")
}

impl Settings {
    /// Load Settings from a given path. Settings are loaded from a given
    /// optional path and can be overridden with environment variables.
    ///
    /// Environment overrides have the same name as the entries in the settings
    /// file in uppercase and prefixed with "PACKET_VERIFY_". For example
    /// "PACKET_VERIFY_DATABASE_URL" will override the database url.
    pub fn new<P: AsRef<Path>>(path: Option<P>) -> Result<Self, config::ConfigError> {
        let mut builder = Config::builder();

        if let Some(file) = path {
            builder = builder
                .add_source(File::with_name(&file.as_ref().to_string_lossy()).required(false));
        }
        builder
            .add_source(Environment::with_prefix("PACKET_VERIFY").separator("__"))
            .build()
            .and_then(|config| config.try_deserialize())
    }
}

use config::{Config, Environment, File};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Clone)]
pub struct Settings {
    /// RUST_LOG compatible settings string. Default to
    /// "iot_verifier=debug"
    #[serde(default = "default_log")]
    pub log: String,
    #[serde(default)]
    pub custom_tracing: custom_tracing::Settings,

    pub ingest_bucket: file_store::BucketSettings,

    pub database: db_store::Settings,

    /// Folder for local cache of backfill state, including the iceberg
    /// `BatchedWriter` spool. Persisted between runs so any pending
    /// (un-committed) records survive a restart and are replayed on
    /// startup.
    #[serde(default = "default_cache")]
    pub cache: PathBuf,

    /// Iceberg connection settings (REST catalog, S3, auth). Required by the
    /// `backfill-rewards` and `backfill-burns` subcommands; optional for
    /// `server`.
    #[serde(default)]
    pub iceberg_settings: Option<helium_iceberg::Settings>,
}

fn default_log() -> String {
    "iot_verifier=debug".to_string()
}

fn default_cache() -> PathBuf {
    PathBuf::from("/var/data/iot-verifier-backfill")
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
            // Add optional settings file
            builder = builder
                .add_source(File::with_name(&file.as_ref().to_string_lossy()).required(false));
        }
        // Add in settings from the environment (with a prefix of VERIFY)
        // Eg.. `INJECT_DEBUG=1 ./target/app` would set the `debug` key
        builder
            .add_source(Environment::with_prefix("VERIFY").separator("_"))
            .build()
            .and_then(|config| config.try_deserialize())
    }
}

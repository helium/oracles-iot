use config::{Config, Environment, File};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Settings {
    #[serde(default = "default_log")]
    pub log: String,
    #[serde(default)]
    pub custom_tracing: custom_tracing::Settings,
    /// Listen address for gRPC entropy requests. Default "0.0.0.0:8080"
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default)]
    pub metrics: poc_metrics::Settings,
}

fn default_log() -> String {
    "poc_entropy=debug".to_string()
}

fn default_listen() -> String {
    "0.0.0.0:8080".to_string()
}

impl Settings {
    pub fn new<P: AsRef<Path>>(path: Option<P>) -> Result<Self, config::ConfigError> {
        let mut builder = Config::builder();
        if let Some(file) = path {
            builder = builder
                .add_source(File::with_name(&file.as_ref().to_string_lossy()).required(false));
        }
        builder
            .add_source(Environment::with_prefix("ENTROPY").separator("__"))
            .build()
            .and_then(|config| config.try_deserialize())
    }
}

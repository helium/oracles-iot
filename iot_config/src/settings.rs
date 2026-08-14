use config::{Config, Environment, File};
use humantime_serde::re::humantime;
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, path::Path, str::FromStr, sync::Arc, time::Duration};

/// Deliberately not `Debug`. These settings reach secrets — `database.url` carries the
/// postgres password and `trino.auth` carries a JWT or basic-auth password — and the
/// crates they come from defend only the `Serialize` path, via `skip_serializing`.
/// `Debug` honours none of that, so a single `{:?}` would print all of it. Log these
/// with `serde_json::to_string_pretty` instead.
#[derive(Deserialize, Serialize)]
pub struct Settings {
    /// RUST_LOG compatible settings string. Default to
    /// "iot_config=info"
    #[serde(default = "default_log")]
    pub log: String,
    #[serde(default)]
    pub custom_tracing: custom_tracing::Settings,
    /// Listen address. Required. Default is 0.0.0.0:8080
    #[serde(default = "default_listen_addr")]
    pub listen: SocketAddr,
    /// Base64-encoded bytes of the config server signing keypair. Can be set in
    /// the settings file or overridden via the `CFG`-prefixed environment
    /// variable (see [`Settings::new`]), so no key file needs to be mounted.
    /// Never serialized: this is the private signing key. The daemon logs the
    /// corresponding public key separately.
    #[serde(
        deserialize_with = "crate::deserialize_helium_keypair",
        skip_serializing
    )]
    pub keypair: Arc<helium_crypto::Keypair>,
    /// B58 encoded public key of the admin keypair
    pub admin: String,
    #[serde(with = "humantime_serde", default = "default_deleted_entry_retention")]
    pub deleted_entry_retention: Duration,
    pub database: db_store::Settings,
    #[serde(with = "humantime_serde", default = "default_gateway_tracker_interval")]
    pub gateway_tracker_interval: std::time::Duration,
    /// Trino query client. Required: the gateway tracker reads the on-chain hotspot
    /// inventory from it, and the sub-DAO service reads epoch reward info from it.
    /// The cluster must expose both the `network` catalog (Iceberg, holding
    /// `chain.iot_hotspot_inventory`) and the `solana` catalog (the on-chain indexer
    /// Postgres, holding `public.sub_dao_epoch_infos`).
    pub trino: trino_client::Settings,
    pub metrics: poc_metrics::Settings,
}

fn default_log() -> String {
    "iot_config=debug".to_string()
}

fn default_listen_addr() -> SocketAddr {
    "0.0.0.0:8080".parse().unwrap()
}

fn default_deleted_entry_retention() -> Duration {
    humantime::parse_duration("48 hours").unwrap()
}

fn default_gateway_tracker_interval() -> std::time::Duration {
    humantime::parse_duration("1 hour").unwrap()
}

impl Settings {
    /// Settings can be loaded from a given optional path and
    /// can be overridden with environment variables.
    ///
    /// Environment overrides have the same name as the entries
    /// in the settings file in uppercase, prefixed with "CFG" and
    /// separated by "__". Example: "CFG__DATABASE__URL" overrides the
    /// database url, "CFG__KEYPAIR" overrides the signing keypair.
    pub fn new<P: AsRef<Path>>(path: Option<P>) -> Result<Self, config::ConfigError> {
        let mut builder = Config::builder();

        if let Some(file) = path {
            // Add optional file
            builder = builder
                .add_source(File::with_name(&file.as_ref().to_string_lossy()).required(false));
        }

        // Add in settings from the environment (with prefix of APP)
        // E.g. `CFG_DEBUG=1 .target/app` would set the `debug` key
        builder
            .add_source(Environment::with_prefix("CFG").separator("__"))
            .build()
            .and_then(|config| config.try_deserialize())
    }

    pub fn signing_keypair(&self) -> Arc<helium_crypto::Keypair> {
        self.keypair.clone()
    }

    pub fn admin_pubkey(&self) -> Result<helium_crypto::PublicKey, helium_crypto::Error> {
        helium_crypto::PublicKey::from_str(&self.admin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use helium_crypto::{KeyTag, Keypair};
    use std::io::Write;

    const JWT: &str = "ey.super.secret.jwt.token";
    const DB_PASSWORD: &str = "hunter2";

    /// Serializing `Settings` is how the daemon logs them at boot, so nothing secret
    /// may survive the round trip. This guards a failure mode that is silent when it
    /// regresses: deriving `Debug` and printing `{:?}` would leak every one of these,
    /// because `skip_serializing` has no effect on `Debug`.
    #[test]
    fn serialized_settings_carry_no_secrets() {
        let keypair = Keypair::generate(KeyTag::default(), &mut rand::rngs::OsRng);
        let admin = keypair.public_key().to_string();
        let encoded = base64::engine::general_purpose::STANDARD.encode(keypair.to_vec());

        let mut file = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .expect("temp settings file");
        write!(
            file,
            r#"
            keypair = "{encoded}"
            admin = "{admin}"

            [database]
            url = "postgres://postgres:{DB_PASSWORD}@127.0.0.1:5432/config_db"

            [trino]
            host = "trino.example.com"
            port = 443
            user = "iot-config"
            secure = true

            [trino.auth]
            type = "jwt"
            token = "{JWT}"

            [metrics]
            endpoint = "127.0.0.1:19000"
            "#
        )
        .expect("write settings");

        let settings = Settings::new(Some(file.path())).expect("load settings");

        // Sanity first: the secrets really are in the loaded struct. Without this a
        // fixture that silently failed to set them would make the assertions below
        // pass while proving nothing.
        assert_eq!(settings.admin, admin);
        assert!(
            settings
                .database
                .url
                .as_deref()
                .is_some_and(|url| url.contains(DB_PASSWORD)),
            "fixture did not load the database password"
        );
        assert!(
            matches!(&settings.trino.auth, Some(trino_client::AuthSettings::Jwt { token }) if token == JWT),
            "fixture did not load the trino jwt"
        );

        let json = serde_json::to_string_pretty(&settings).expect("serialize settings");

        assert!(
            !json.contains(JWT),
            "trino jwt leaked into settings log:\n{json}"
        );
        assert!(
            !json.contains(DB_PASSWORD),
            "database password leaked into settings log:\n{json}"
        );
        assert!(
            !json.contains(&encoded),
            "signing keypair leaked into settings log:\n{json}"
        );
        // ...while the non-secret configuration is still there to be useful.
        assert!(json.contains("trino.example.com"), "{json}");
        assert!(json.contains(&admin), "{json}");
    }
}

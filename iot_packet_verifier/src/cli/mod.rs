pub mod backfill_valid_packets;
pub mod server;

use crate::{backfill, settings::Settings};
use anyhow::Result;
use std::path;

#[derive(Debug, clap::Parser)]
#[clap(version = env!("CARGO_PKG_VERSION"))]
#[clap(about = "Helium IOT Packet Verifier")]
pub struct Cli {
    /// Optional configuration file to use. If present the toml file at the
    /// given path will be loaded. Environment variables can override the
    /// settings in the given file.
    #[clap(short = 'c')]
    config: Option<path::PathBuf>,

    #[clap(subcommand)]
    cmd: Cmd,
}

impl Cli {
    pub async fn run(self) -> Result<()> {
        self.cmd.run(self.config).await
    }
}

#[derive(Debug, clap::Subcommand)]
pub enum Cmd {
    Server(server::Cmd),
    BackfillValidPackets(backfill_valid_packets::Cmd),
}

impl Cmd {
    pub async fn run(self, config: Option<path::PathBuf>) -> Result<()> {
        match self {
            Self::Server(cmd) => {
                let settings = Settings::new(config)?;
                cmd.run(settings).await
            }
            Self::BackfillValidPackets(cmd) => {
                let settings = backfill::settings::Settings::new(config)?;
                cmd.run(&settings).await
            }
        }
    }
}

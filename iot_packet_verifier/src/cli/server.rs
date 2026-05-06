use crate::{daemon, settings::Settings};
use anyhow::Result;

#[derive(Debug, clap::Args)]
pub struct Cmd {}

impl Cmd {
    pub async fn run(self, settings: Settings) -> Result<()> {
        custom_tracing::init(settings.log.clone(), settings.custom_tracing.clone()).await?;
        daemon::Cmd {}.run(settings).await
    }
}

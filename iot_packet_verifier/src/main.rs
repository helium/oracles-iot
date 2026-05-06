use anyhow::Result;
use clap::Parser;
use iot_packet_verifier::cli::Cli;

#[tokio::main]
async fn main() -> Result<()> {
    Cli::parse().run().await
}

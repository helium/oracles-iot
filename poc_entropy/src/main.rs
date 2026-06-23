extern crate tls_init;

mod settings;

use anyhow::Result;
use chrono::Utc;
use clap::Parser;
use helium_proto::{
    services::poc_entropy::{EntropyReqV1, PocEntropy, Server as GrpcServer},
    EntropyReportV1,
};
use std::{net::SocketAddr, path};
use tokio::signal;
use tonic::{transport, Request, Response, Status};

#[derive(Debug, Parser)]
#[clap(version = env!("CARGO_PKG_VERSION"))]
#[clap(about = "Helium PoC Entropy Server (noop — POC retired per HIP-0149)")]
pub struct Cli {
    #[clap(short = 'c')]
    config: Option<path::PathBuf>,

    #[clap(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, clap::Subcommand)]
pub enum Cmd {
    Server(Server),
}

#[derive(Debug, clap::Args)]
pub struct Server {}

impl Server {
    async fn run(&self, settings: &settings::Settings) -> Result<()> {
        poc_metrics::start_metrics(&settings.metrics)?;

        let (shutdown_trigger, shutdown) = triggered::trigger();
        let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())?;
        tokio::spawn(async move {
            tokio::select! {
                _ = sigterm.recv() => shutdown_trigger.trigger(),
                _ = signal::ctrl_c() => shutdown_trigger.trigger(),
            }
        });

        let socket_addr: SocketAddr = settings.listen.parse()?;
        tracing::info!(%socket_addr, "starting noop entropy server (POC retired)");

        transport::Server::builder()
            .layer(custom_tracing::grpc_layer::new_with_span(make_span))
            .add_service(GrpcServer::new(NoopEntropyServer))
            .serve_with_shutdown(socket_addr, shutdown)
            .await?;

        Ok(())
    }
}

struct NoopEntropyServer;

#[tonic::async_trait]
impl PocEntropy for NoopEntropyServer {
    async fn entropy(
        &self,
        _request: Request<EntropyReqV1>,
    ) -> Result<Response<EntropyReportV1>, Status> {
        // POC retired (HIP-0149). Return a valid but empty response so old
        // gateway firmware that cannot be updated doesn't get connection errors.
        // Gateways are expected to discard this entropy since beacons are no
        // longer validated.
        Ok(Response::new(EntropyReportV1 {
            data: vec![],
            timestamp: Utc::now().timestamp() as u64,
            version: 0,
        }))
    }
}

fn make_span(_request: &http::request::Request<tonic::body::Body>) -> tracing::Span {
    tracing::info_span!(custom_tracing::DEFAULT_SPAN)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let settings = settings::Settings::new(cli.config)?;
    custom_tracing::init(settings.log.clone(), settings.custom_tracing.clone()).await?;
    match cli.cmd {
        Cmd::Server(cmd) => cmd.run(&settings).await,
    }
}

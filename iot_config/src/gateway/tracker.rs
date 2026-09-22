use crate::gateway::{db::Gateway, trino};
use futures::StreamExt;
use sqlx::{Pool, Postgres};
use std::time::{Duration, Instant};
use task_manager::ManagedTask;

const EXECUTE_DURATION_METRIC: &str =
    concat!(env!("CARGO_PKG_NAME"), "-", "tracker-execute-duration");
/// Incremented whenever a tick fails. The duration histogram is only recorded on
/// success, so without this a permanently broken tracker (expired JWT, renamed
/// catalog) would emit no metric at all and be indistinguishable from a scrape gap
/// while `gateways` silently goes stale.
const EXECUTE_FAILURE_METRIC: &str =
    concat!(env!("CARGO_PKG_NAME"), "-", "tracker-execute-failures");

/// Rows per `INSERT ... ON CONFLICT` statement.
const BATCH_SIZE: usize = 1_000;

pub struct Tracker {
    pool: Pool<Postgres>,
    trino: trino_client::Client,
    inventory_table: String,
    interval: Duration,
}

impl ManagedTask for Tracker {
    fn start_task(self: Box<Self>, shutdown: triggered::Listener) -> task_manager::TaskFuture {
        task_manager::spawn(self.run(shutdown))
    }
}

impl Tracker {
    pub fn new(pool: Pool<Postgres>, trino: trino_client::Client, interval: Duration) -> Self {
        Self {
            pool,
            trino,
            inventory_table: trino::IOT_HOTSPOT_INVENTORY_TABLE.to_string(),
            interval,
        }
    }

    async fn run(self, mut shutdown: triggered::Listener) -> anyhow::Result<()> {
        tracing::info!("starting with interval: {:?}", self.interval);
        let mut interval = tokio::time::interval(self.interval);

        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown => break,
                _ = interval.tick() => {
                    if let Err(err) = execute(&self.pool, &self.trino, &self.inventory_table).await {
                        // A Trino hiccup shouldn't take the daemon down; the next
                        // tick retries against unchanged local data. Counted so a
                        // persistent failure is alertable rather than just a log line.
                        metrics::counter!(EXECUTE_FAILURE_METRIC).increment(1);
                        tracing::error!(?err, "tracker execute failed");
                    }
                }
            }
        }

        tracing::info!("stopping");

        Ok(())
    }
}

pub async fn execute(
    pool: &Pool<Postgres>,
    trino: &trino_client::Client,
    inventory_table: &str,
) -> anyhow::Result<()> {
    tracing::info!("starting execute");
    let start = Instant::now();

    let mut total: u64 = 0;

    let batches = trino::stream_gateways(trino, inventory_table, BATCH_SIZE);
    futures::pin_mut!(batches);

    while let Some(batch) = batches.next().await {
        // A Trino failure aborts the tick; a bad insert is logged and the rest of
        // the inventory still lands.
        let batch = batch?;
        match Gateway::insert_bulk(pool, &batch).await {
            Ok(affected) => total += affected,
            Err(err) => tracing::error!(?err, "failed to insert gateway batch"),
        }
    }

    let elapsed = start.elapsed();
    tracing::info!(?elapsed, affected = total, "done execute");
    metrics::histogram!(EXECUTE_DURATION_METRIC).record(elapsed);

    Ok(())
}

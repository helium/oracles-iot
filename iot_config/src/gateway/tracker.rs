use crate::gateway::{db::Gateway, trino};
use sqlx::{Pool, Postgres};
use std::time::{Duration, Instant};
use task_manager::ManagedTask;

const EXECUTE_DURATION_METRIC: &str =
    concat!(env!("CARGO_PKG_NAME"), "-", "tracker-execute-duration");

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
        Self::new_with_inventory_table(pool, trino, trino::IOT_HOTSPOT_INVENTORY_TABLE, interval)
    }

    /// Like [`new`](Self::new), but with an explicit inventory table name. Tests use
    /// this to point at a seeded table in their own catalog, since the production
    /// `network.chain.iot_hotspot_inventory` name does not exist there.
    pub fn new_with_inventory_table(
        pool: Pool<Postgres>,
        trino: trino_client::Client,
        inventory_table: impl Into<String>,
        interval: Duration,
    ) -> Self {
        Self {
            pool,
            trino,
            inventory_table: inventory_table.into(),
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
                        // tick retries against unchanged local data.
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

    let mut after = String::new();
    let mut total: u64 = 0;

    loop {
        let rows = trino::fetch_page(trino, inventory_table, &after).await?;
        let Some(cursor) = trino::page_cursor(&rows) else {
            break;
        };
        after = cursor;

        for batch in trino::page_to_gateways(&rows).chunks(BATCH_SIZE) {
            match Gateway::insert_bulk(pool, batch).await {
                Ok(affected) => total += affected,
                Err(err) => tracing::error!(?err, "failed to insert gateway batch"),
            }
        }
    }

    let elapsed = start.elapsed();
    tracing::info!(?elapsed, affected = total, "done execute");
    metrics::histogram!(EXECUTE_DURATION_METRIC).record(elapsed);

    Ok(())
}

// TODO cleanup imports in all the files in the end
use cached::SizedCache;
use clap::Parser;
use dotenv::dotenv;
use futures::{try_join, StreamExt};
use std::env;
use tokio::sync::Mutex;
use tracing_subscriber::EnvFilter;

use crate::configs::Opts;

mod configs;
mod db_adapters;
mod models;

// Categories for logging
// TODO naming
pub(crate) const INDEXER: &str = "indexer";

const INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
const MAX_DELAY_TIME: std::time::Duration = std::time::Duration::from_secs(120);

#[derive(Clone, Hash, PartialEq, Eq, Debug)]
pub enum ReceiptOrDataId {
    ReceiptId(near_indexer_primitives::CryptoHash),
    DataId(near_indexer_primitives::CryptoHash),
}
// Creating type aliases to make HashMap types for cache more explicit
pub type ParentTransactionHashString = String;
// Introducing a simple cache for Receipts to find their parent Transactions without
// touching the database
// The key is ReceiptID
// The value is TransactionHash (the very parent of the Receipt)
pub type ReceiptsCache =
    std::sync::Arc<Mutex<SizedCache<ReceiptOrDataId, ParentTransactionHashString>>>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();

    let opts: Opts = Opts::parse();

    let options = sqlx::postgres::PgConnectOptions::new()
        .host(&env::var("DB_HOST")?)
        .port(env::var("DB_PORT")?.parse()?)
        .username(&env::var("DB_USER")?)
        .password(&env::var("DB_PASSWORD")?)
        .database(&env::var("DB_NAME")?)
        .extra_float_digits(2);

    let pool = sqlx::PgPool::connect_with(options).await?;
    // let pool = sqlx::PgPool::connect(&env::var("DATABASE_URL")?).await?;
    // let pool = sqlx::PgPool::connect(&env::var("DATABASE_URL")?).await?;
    // TODO Error: while executing migrations: error returned from database: 1128 (HY000): Function 'near_indexer.GET_LOCK' is not defined
    // sqlx::migrate!().run(&pool).await?;

    let start_block_height = match opts.start_block_height {
        Some(x) => x,
        None => models::start_after_interruption(&pool).await?,
    };
    let config = near_lake_framework::LakeConfig {
        s3_endpoint: None,
        s3_bucket_name: opts.s3_bucket_name.clone(),
        s3_region_name: opts.s3_region_name.clone(),
        start_block_height,
    };
    init_tracing();

    let stream = near_lake_framework::streamer(config);

    // We want to prevent unnecessary SELECT queries to the database to find
    // the Transaction hash for the Receipt.
    // Later we need to find the Receipt which is a parent to underlying Receipts.
    // Receipt ID will of the child will be stored as key and parent Transaction hash/Receipt ID
    // will be stored as a value
    let receipts_cache: ReceiptsCache =
        std::sync::Arc::new(Mutex::new(SizedCache::with_size(100_000)));

    let mut handlers = tokio_stream::wrappers::ReceiverStream::new(stream)
        .map(|streamer_message| {
            handle_streamer_message(
                streamer_message,
                &pool,
                std::sync::Arc::clone(&receipts_cache),
                !opts.non_strict_mode,
            )
        })
        .buffer_unordered(100usize);

    let mut time_now = std::time::Instant::now();
    while let Some(handle_message) = handlers.next().await {
        match handle_message {
            Ok(block_height) => {
                let elapsed = time_now.elapsed();
                println!(
                    "Elapsed time spent on block {}: {:.3?}",
                    block_height, elapsed
                );
                time_now = std::time::Instant::now();
            }
            Err(e) => {
                return Err(anyhow::anyhow!(e));
            }
        }
    }

    Ok(())
}

async fn handle_streamer_message(
    streamer_message: near_lake_framework::near_indexer_primitives::StreamerMessage,
    pool: &sqlx::Pool<sqlx::Postgres>,
    receipts_cache: ReceiptsCache,
    strict_mode: bool,
) -> anyhow::Result<u64> {
    eprintln!(
        "{} / shards {}",
        streamer_message.block.header.height,
        streamer_message.shards.len()
    );

    let blocks_future = db_adapters::blocks::store_block(pool, &streamer_message.block);

    let chunks_future = db_adapters::chunks::store_chunks(
        pool,
        &streamer_message.shards,
        &streamer_message.block.header.hash,
        streamer_message.block.header.timestamp,
    );

    let transactions_future = db_adapters::transactions::store_transactions(
        pool,
        &streamer_message.shards,
        &streamer_message.block.header.hash,
        streamer_message.block.header.timestamp,
        std::sync::Arc::clone(&receipts_cache),
    );

    let receipts_future = db_adapters::receipts::store_receipts(
        pool,
        strict_mode,
        &streamer_message.shards,
        &streamer_message.block.header.hash,
        streamer_message.block.header.timestamp,
        streamer_message.block.header.height,
        std::sync::Arc::clone(&receipts_cache),
    );

    let execution_outcomes_future = db_adapters::execution_outcomes::store_execution_outcomes(
        pool,
        &streamer_message.shards,
        &streamer_message.block.header.hash,
        streamer_message.block.header.timestamp,
        std::sync::Arc::clone(&receipts_cache),
    );

    let account_changes_future = db_adapters::account_changes::store_account_changes(
        pool,
        &streamer_message.shards,
        &streamer_message.block.header.hash,
        streamer_message.block.header.timestamp,
    );

    try_join!(blocks_future, chunks_future, transactions_future)?;
    try_join!(receipts_future)?; // this guy can contain local receipts, so we have to do that after transactions_future finished the work
    try_join!(execution_outcomes_future, account_changes_future)?; // this guy thinks that receipts_future finished, and clears the cache
    Ok(streamer_message.block.header.height)
}

fn init_tracing() {
    let mut env_filter = EnvFilter::new("near_lake_framework=info");

    if let Ok(rust_log) = std::env::var("RUST_LOG") {
        if !rust_log.is_empty() {
            for directive in rust_log.split(',').filter_map(|s| match s.parse() {
                Ok(directive) => Some(directive),
                Err(err) => {
                    eprintln!("Ignoring directive `{}`: {}", s, err);
                    None
                }
            }) {
                env_filter = env_filter.add_directive(directive);
            }
        }
    }

    tracing_subscriber::fmt::Subscriber::builder()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .init();
}
//! Attesta chain indexer.
//!
//! Polls Soroban RPC `getEvents` for the shielded-pool and registry
//! contracts and mirrors public state into Postgres: commitment tree
//! leaves, nullifiers, encrypted note blobs, issuer registry entries,
//! and public pool totals.
//!
//! Everything indexed is public chain data or ciphertext. All state is
//! replayable: wiping the database and restarting from ledger 0 always
//! reproduces it.

mod events;
mod ingest;
mod rpc;

use attesta_core::{config::Config, db};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env()?;
    let pool = db::connect(&config.database_url).await?;
    db::migrate(&pool).await?;

    if config.pool_contract_ids.is_empty() && config.registry_contract_id.is_none() {
        tracing::warn!(
            "no contracts configured (POOL_CONTRACT_IDS / REGISTRY_CONTRACT_ID); \
             indexer will idle until they are set"
        );
    }

    let client = rpc::SorobanClient::new(config.soroban_rpc_url.clone());
    match client.latest_ledger().await {
        Ok(l) => {
            tracing::info!(rpc = %config.soroban_rpc_url, ledger = l.sequence, "connected to Soroban RPC")
        }
        Err(e) => {
            tracing::warn!(rpc = %config.soroban_rpc_url, error = %e, "Soroban RPC unreachable at startup; will keep retrying")
        }
    }
    ingest::run(pool, client, config).await
}

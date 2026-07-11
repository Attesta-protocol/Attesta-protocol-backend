use std::env;

/// Runtime configuration, read from the environment (see `.env.example`).
#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub bind_addr: String,
    pub soroban_rpc_url: String,
    pub network_passphrase: String,
    pub pool_contract_ids: Vec<String>,
    pub registry_contract_id: Option<String>,
    pub indexer_poll_secs: u64,
    pub artifacts_dir: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            database_url: env::var("DATABASE_URL")
                .map_err(|_| anyhow::anyhow!("DATABASE_URL must be set"))?,
            bind_addr: env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into()),
            soroban_rpc_url: env::var("SOROBAN_RPC_URL")
                .unwrap_or_else(|_| "https://soroban-testnet.stellar.org".into()),
            network_passphrase: env::var("STELLAR_NETWORK_PASSPHRASE")
                .unwrap_or_else(|_| "Test SDF Network ; September 2015".into()),
            pool_contract_ids: env::var("POOL_CONTRACT_IDS")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
            registry_contract_id: env::var("REGISTRY_CONTRACT_ID")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            indexer_poll_secs: env::var("INDEXER_POLL_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(5),
            artifacts_dir: env::var("ARTIFACTS_DIR").unwrap_or_else(|_| "./artifacts".into()),
        })
    }
}

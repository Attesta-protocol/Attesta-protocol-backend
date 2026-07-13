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
    /// Days after `claimed_at` before a claimed delivery is deleted.
    /// 0 disables deletion of claimed rows.
    pub credential_retention_claimed_days: u32,
    /// Days after `created_at` before an unclaimed delivery is deleted.
    /// 0 disables deletion of unclaimed rows.
    pub credential_retention_unclaimed_days: u32,
    /// Abuse-protection knobs. Every limit can be disabled (0) for private
    /// deployments; defaults assume a publicly exposed self-hosted API.
    pub rate_limits: RateLimits,
    /// Origins allowed for browser (CORS) access. Empty = no CORS layer.
    /// `*` = any origin (public read API behind a proxy).
    pub cors_allowed_origins: Vec<String>,
    /// /health/ready also fails if no indexer cursor was updated within
    /// this many seconds. 0 disables the staleness check (API-only
    /// deployments, or before any contracts are configured).
    pub ready_max_indexer_staleness_secs: u32,
}

/// Per-IP token buckets and quotas. A value of 0 disables that limit.
#[derive(Debug, Clone)]
pub struct RateLimits {
    /// Sustained read requests per second per IP.
    pub read_per_sec: u32,
    /// Read burst size per IP.
    pub read_burst: u32,
    /// Sustained write requests per second per IP.
    pub write_per_sec: u32,
    /// Write burst size per IP.
    pub write_burst: u32,
    /// Max concurrent SSE connections per IP.
    pub sse_per_ip: u32,
    /// Max concurrent SSE connections in total.
    pub sse_global: u32,
    /// Max credential deliveries per issuer per hour.
    pub issuer_deliveries_per_hour: u32,
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
            credential_retention_claimed_days: env_u32("CREDENTIAL_RETENTION_CLAIMED_DAYS", 30),
            credential_retention_unclaimed_days: env_u32(
                "CREDENTIAL_RETENTION_UNCLAIMED_DAYS",
                180,
            ),
            rate_limits: RateLimits {
                read_per_sec: env_u32("RATE_LIMIT_READ_PER_SEC", 50),
                read_burst: env_u32("RATE_LIMIT_READ_BURST", 200),
                write_per_sec: env_u32("RATE_LIMIT_WRITE_PER_SEC", 2),
                write_burst: env_u32("RATE_LIMIT_WRITE_BURST", 20),
                sse_per_ip: env_u32("RATE_LIMIT_SSE_PER_IP", 10),
                sse_global: env_u32("RATE_LIMIT_SSE_GLOBAL", 1000),
                issuer_deliveries_per_hour: env_u32("RATE_LIMIT_ISSUER_DELIVERIES_PER_HOUR", 600),
            },
            cors_allowed_origins: env::var("CORS_ALLOWED_ORIGINS")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
            ready_max_indexer_staleness_secs: env_u32("READY_MAX_INDEXER_STALENESS_SECS", 0),
        })
    }
}

fn env_u32(key: &str, default: u32) -> u32 {
    env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

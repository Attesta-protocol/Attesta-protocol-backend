//! Attesta backend API.
//!
//! Serves Merkle paths to provers, relays encrypted note blobs, accepts
//! issuer credential deliveries (ciphertext only), and exposes public
//! protocol stats and prover artifacts.
//!
//! Hard invariant: no endpoint accepts a plaintext amount, a spending key,
//! or an unencrypted credential. Ciphertext in, ciphertext out.

mod error;
mod limits;
mod retention;
mod routes;
mod state;

use std::sync::Arc;

use attesta_core::{config::Config, db};
use axum::http::HeaderValue;
use tokio::sync::broadcast;
use tower_http::{
    cors::{AllowOrigin, Any, CorsLayer},
    limit::RequestBodyLimitLayer,
    trace::TraceLayer,
};
use tracing_subscriber::EnvFilter;

use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env()?;
    let pool = db::connect(&config.database_url).await?;
    db::migrate(&pool).await?;

    // Note fan-out channel: a lightweight poller watches the encrypted_notes
    // table and broadcasts new rows to SSE subscribers.
    // Prometheus exposition on GET /metrics. Only pool ids, counts, and
    // timings are ever recorded — never per-user data (see docs/operations.md).
    let metrics = metrics_exporter_prometheus::PrometheusBuilder::new().install_recorder()?;
    tokio::spawn({
        let handle = metrics.clone();
        async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                handle.run_upkeep();
            }
        }
    });

    let (note_tx, _) = broadcast::channel(1024);
    let rl = &config.rate_limits;
    let state = Arc::new(AppState {
        db: pool,
        config: config.clone(),
        metrics,
        note_tx,
        read_buckets: limits::IpBuckets::new(rl.read_per_sec, rl.read_burst),
        write_buckets: limits::IpBuckets::new(rl.write_per_sec, rl.write_burst),
        sse_slots: Arc::new(limits::SseSlots::new(rl.sse_per_ip, rl.sse_global)),
        trees: Default::default(),
    });

    tokio::spawn(routes::notes::poll_new_notes(state.clone()));
    tokio::spawn(retention::run(state.clone()));

    let mut app = routes::router(state)
        .layer(RequestBodyLimitLayer::new(256 * 1024)) // ciphertext blobs are small
        .layer(TraceLayer::new_for_http());

    // Browser provers need CORS; off unless origins are configured.
    // `CORS_ALLOWED_ORIGINS=*` opens every origin (read API is public
    // data anyway); otherwise an explicit allowlist.
    if let Some(cors) = cors_layer(&config.cors_allowed_origins)? {
        app = app.layer(cors);
    }

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(addr = %config.bind_addr, "attesta-api listening");
    // ConnectInfo gives the rate limiter each client's peer address.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

fn cors_layer(origins: &[String]) -> anyhow::Result<Option<CorsLayer>> {
    if origins.is_empty() {
        return Ok(None);
    }
    let allow_origin = if origins.iter().any(|o| o == "*") {
        AllowOrigin::any()
    } else {
        let parsed: Vec<HeaderValue> = origins
            .iter()
            .map(|o| {
                o.parse()
                    .map_err(|_| anyhow::anyhow!("invalid origin in CORS_ALLOWED_ORIGINS: {o}"))
            })
            .collect::<anyhow::Result<_>>()?;
        AllowOrigin::list(parsed)
    };
    Ok(Some(
        CorsLayer::new()
            .allow_origin(allow_origin)
            .allow_methods(Any)
            .allow_headers(Any),
    ))
}

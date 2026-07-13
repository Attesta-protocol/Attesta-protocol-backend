pub mod artifacts;
pub mod issuer;
pub mod notes;
pub mod stats;
pub mod tree;

use std::sync::Arc;

use axum::{
    middleware,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;

use crate::{limits, state::AppState};

pub fn router(state: Arc<AppState>) -> Router {
    // Reads and writes get separate per-IP budgets so a throttled writer
    // cannot starve reads (and vice versa). The SSE route manages its own
    // concurrent-connection slots instead of a request-rate bucket.
    let reads = Router::new()
        .route("/v1/tree/{pool}/path", get(tree::get_path))
        .route("/v1/tree/{pool}/root", get(tree::get_root))
        .route("/v1/notes", get(notes::list_notes))
        .route("/v1/credentials", get(issuer::list_deliveries))
        .route("/v1/issuers", get(issuer::list_issuers))
        .route("/v1/stats", get(stats::get_stats))
        .route(
            "/v1/artifacts/{circuit}/{version}",
            get(artifacts::get_manifest),
        )
        .route(
            "/v1/artifacts/{circuit}/{version}/{file}",
            get(artifacts::get_file),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            limits::limit_reads,
        ));

    let writes = Router::new()
        .route("/v1/issuer/credentials", post(issuer::deliver_credential))
        .route(
            "/v1/credentials/{delivery_id}/claim",
            post(issuer::claim_delivery),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            limits::limit_writes,
        ));

    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/v1/notes/stream", get(notes::stream_notes))
        .merge(reads)
        .merge(writes)
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "service": "attesta-api" }))
}

/// Prometheus exposition. Pool ids, counts, and timings only — no secrets,
/// no per-user data (invariant restated in docs/operations.md).
async fn metrics(axum::extract::State(state): axum::extract::State<Arc<AppState>>) -> String {
    state.metrics.render()
}

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
        .route("/health", get(health_live)) // back-compat alias for liveness
        .route("/health/live", get(health_live))
        .route("/health/ready", get(health_ready))
        .route("/metrics", get(metrics))
        .route("/v1/notes/stream", get(notes::stream_notes))
        .merge(reads)
        .merge(writes)
        .with_state(state)
}

/// Liveness: the process is up. Never touches the database.
async fn health_live() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "service": "attesta-api" }))
}

/// Readiness: the database answers (migrations ran at startup, so a
/// reachable database is a migrated one), and — if
/// READY_MAX_INDEXER_STALENESS_SECS is set — some indexer cursor moved
/// recently. 503 with the failing check named, 200 otherwise; recovers
/// without a process restart.
async fn health_ready(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> (axum::http::StatusCode, Json<serde_json::Value>) {
    use axum::http::StatusCode;

    if let Err(e) = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.db)
        .await
    {
        tracing::warn!(error = %e, "readiness: database unreachable");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "unavailable", "failing": "database" })),
        );
    }

    let max_staleness = state.config.ready_max_indexer_staleness_secs;
    if max_staleness > 0 {
        let fresh: Option<bool> = sqlx::query_scalar(
            "SELECT max(updated_at) > now() - make_interval(secs => $1)
             FROM indexer_cursors",
        )
        .bind(max_staleness as f64)
        .fetch_one(&state.db)
        .await
        .unwrap_or(Some(false));
        if fresh != Some(true) {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "status": "unavailable", "failing": "indexer_staleness" })),
            );
        }
    }

    (StatusCode::OK, Json(json!({ "status": "ready" })))
}

/// Prometheus exposition. Pool ids, counts, and timings only — no secrets,
/// no per-user data (invariant restated in docs/operations.md).
async fn metrics(axum::extract::State(state): axum::extract::State<Arc<AppState>>) -> String {
    state.metrics.render()
}

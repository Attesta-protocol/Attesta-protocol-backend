pub mod artifacts;
pub mod issuer;
pub mod notes;
pub mod stats;
pub mod tree;

use std::sync::Arc;

use axum::{
    routing::{get, post},
    Json, Router,
};
use serde_json::json;

use crate::state::AppState;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/tree/{pool}/path", get(tree::get_path))
        .route("/v1/tree/{pool}/root", get(tree::get_root))
        .route("/v1/notes", get(notes::list_notes))
        .route("/v1/notes/stream", get(notes::stream_notes))
        .route("/v1/issuer/credentials", post(issuer::deliver_credential))
        .route("/v1/credentials", get(issuer::list_deliveries))
        .route(
            "/v1/credentials/{delivery_id}/claim",
            post(issuer::claim_delivery),
        )
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
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "service": "attesta-api" }))
}

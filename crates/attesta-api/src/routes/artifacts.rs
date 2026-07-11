//! Prover artifacts CDN: versioned proving keys and WASM prover binaries,
//! each with a published SHA-256 so clients can integrity-check before
//! proving. Layout on disk:
//!
//!   {ARTIFACTS_DIR}/{circuit}/{version}/manifest.json
//!   {ARTIFACTS_DIR}/{circuit}/{version}/<files referenced by the manifest>

use std::{path::PathBuf, sync::Arc};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use sha2::{Digest, Sha256};

use crate::{error::ApiError, state::AppState};

/// GET /v1/artifacts/{circuit}/{version} → the version's manifest
/// (file list + sha256 hashes).
pub async fn get_manifest(
    State(state): State<Arc<AppState>>,
    Path((circuit, version)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let path = artifact_path(&state, &circuit, &version, "manifest.json")?;
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| ApiError::not_found("unknown circuit/version"))?;
    let manifest: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::bad_request("corrupt manifest on server"))?;
    Ok(Json(manifest))
}

/// GET /v1/artifacts/{circuit}/{version}/{file} → the artifact bytes, with
/// its sha256 in the `x-artifact-sha256` header for a second integrity check.
pub async fn get_file(
    State(state): State<Arc<AppState>>,
    Path((circuit, version, file)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    let path = artifact_path(&state, &circuit, &version, &file)?;
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| ApiError::not_found("unknown artifact"))?;
    let digest = hex::encode(Sha256::digest(&bytes));

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (header::HeaderName::from_static("x-artifact-sha256"), digest),
            (
                header::CACHE_CONTROL,
                "public, max-age=31536000, immutable".to_string(),
            ),
        ],
        Body::from(bytes),
    )
        .into_response())
}

/// Build a path strictly inside ARTIFACTS_DIR; each segment must be a plain
/// name (no separators, no traversal).
fn artifact_path(
    state: &AppState,
    circuit: &str,
    version: &str,
    file: &str,
) -> Result<PathBuf, ApiError> {
    for segment in [circuit, version, file] {
        let ok = !segment.is_empty()
            && segment.len() <= 128
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            && !segment.contains("..");
        if !ok {
            return Err(ApiError::bad_request("invalid artifact path segment"));
        }
    }
    Ok(PathBuf::from(&state.config.artifacts_dir)
        .join(circuit)
        .join(version)
        .join(file))
}

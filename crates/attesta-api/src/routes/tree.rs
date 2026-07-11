//! Merkle tree endpoints: paths for provers, current root + block anchor.

use std::sync::Arc;

use attesta_core::merkle::{MerkleTree, Node, Sha256Hasher};
use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{error::ApiError, state::AppState};

#[derive(Deserialize)]
pub struct PathQuery {
    /// 0x-hex commitment whose path is requested.
    pub commitment: String,
}

#[derive(Serialize)]
pub struct PathResponse {
    pub pool: String,
    pub leaf_index: i64,
    pub root: String,
    /// Ledger the newest indexed leaf was observed at — the block anchor
    /// clients pin their proof to.
    pub anchored_ledger: i64,
    pub path: Vec<PathElementJson>,
}

#[derive(Serialize)]
pub struct PathElementJson {
    pub sibling: String,
    pub sibling_on_right: bool,
}

#[derive(Serialize)]
pub struct RootResponse {
    pub pool: String,
    pub root: String,
    pub leaf_count: i64,
    pub anchored_ledger: i64,
}

/// GET /v1/tree/{pool}/path?commitment=0x…
pub async fn get_path(
    State(state): State<Arc<AppState>>,
    Path(pool): Path<String>,
    Query(q): Query<PathQuery>,
) -> Result<Json<PathResponse>, ApiError> {
    let commitment = parse_node(&q.commitment)?;

    let (leaves, anchored_ledger) = load_leaves(&state, &pool).await?;
    let leaf_index = leaves
        .iter()
        .position(|l| *l == commitment)
        .ok_or_else(|| ApiError::not_found("commitment not found in tree"))?;

    let tree = MerkleTree::from_leaves(Sha256Hasher, leaves);
    let path = tree
        .path(leaf_index)
        .ok_or_else(|| ApiError::not_found("commitment not found in tree"))?;

    Ok(Json(PathResponse {
        pool,
        leaf_index: leaf_index as i64,
        root: hex0x(&tree.root()),
        anchored_ledger,
        path: path
            .into_iter()
            .map(|el| PathElementJson {
                sibling: hex0x(&el.sibling),
                sibling_on_right: el.sibling_on_right,
            })
            .collect(),
    }))
}

/// GET /v1/tree/{pool}/root
pub async fn get_root(
    State(state): State<Arc<AppState>>,
    Path(pool): Path<String>,
) -> Result<Json<RootResponse>, ApiError> {
    let (leaves, anchored_ledger) = load_leaves(&state, &pool).await?;
    let leaf_count = leaves.len() as i64;
    let tree = MerkleTree::from_leaves(Sha256Hasher, leaves);
    Ok(Json(RootResponse {
        pool,
        root: hex0x(&tree.root()),
        leaf_count,
        anchored_ledger,
    }))
}

/// Load all leaves for a pool, ordered by leaf_index, plus the newest ledger.
/// Testnet-scale implementation; replaced by a cached frontier later.
async fn load_leaves(state: &AppState, pool: &str) -> Result<(Vec<Node>, i64), ApiError> {
    let rows: Vec<(Vec<u8>, i64)> = sqlx::query_as(
        "SELECT commitment, ledger FROM commitments WHERE pool = $1 ORDER BY leaf_index",
    )
    .bind(pool)
    .fetch_all(&state.db)
    .await?;

    if rows.is_empty() {
        return Err(ApiError::not_found(format!(
            "unknown or empty pool: {pool}"
        )));
    }

    let anchored_ledger = rows.iter().map(|(_, l)| *l).max().unwrap_or(0);
    let leaves = rows
        .into_iter()
        .map(|(bytes, _)| {
            let mut node: Node = [0u8; 32];
            if bytes.len() != 32 {
                return Err(ApiError::bad_request("corrupt commitment in index"));
            }
            node.copy_from_slice(&bytes);
            Ok(node)
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok((leaves, anchored_ledger))
}

fn parse_node(s: &str) -> Result<Node, ApiError> {
    let bytes = hex::decode(s.trim_start_matches("0x"))
        .map_err(|_| ApiError::bad_request("commitment must be 0x-hex"))?;
    if bytes.len() != 32 {
        return Err(ApiError::bad_request("commitment must be 32 bytes"));
    }
    let mut node = [0u8; 32];
    node.copy_from_slice(&bytes);
    Ok(node)
}

fn hex0x(n: &Node) -> String {
    format!("0x{}", hex::encode(n))
}

//! Merkle tree endpoints: paths for provers, current root + block anchor.
//!
//! Trees are cached in memory per pool (see [`PoolTree`]) and topped up
//! with newly indexed leaves on each request, so serving a path costs one
//! small DB query plus O(depth) work instead of a full tree rebuild.

use std::sync::Arc;

use attesta_core::merkle::Node;
use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    error::ApiError,
    state::{AppState, PoolTree},
};

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
    /// Tree size when this path was computed. Lets clients pin
    /// path ↔ root ↔ ledger consistently and re-fetch the same root later
    /// via `/root?at_leaf_count=`.
    pub leaf_count: i64,
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

    let mut trees = lock_trees(&state).await;
    let pool_tree = sync_pool_tree(&state, &mut trees, &pool).await?;

    let leaf_index = *pool_tree
        .index_by_commitment
        .get(&commitment)
        .ok_or_else(|| ApiError::not_found("commitment not found in tree"))?;
    let path = pool_tree
        .tree
        .path(leaf_index)
        .ok_or_else(|| ApiError::not_found("commitment not found in tree"))?;

    Ok(Json(PathResponse {
        pool,
        leaf_index: leaf_index as i64,
        root: hex0x(&pool_tree.tree.root()),
        leaf_count: pool_tree.tree.len() as i64,
        anchored_ledger: pool_tree.anchored_ledger,
        path: path
            .into_iter()
            .map(|el| PathElementJson {
                sibling: hex0x(&el.sibling),
                sibling_on_right: el.sibling_on_right,
            })
            .collect(),
    }))
}

#[derive(Deserialize)]
pub struct RootQuery {
    /// Return the newest root anchored at or before this ledger.
    pub at_ledger: Option<i64>,
    /// Return the root the tree had at exactly this leaf count (or the
    /// newest earlier one if that count fell inside a multi-leaf batch).
    pub at_leaf_count: Option<i64>,
}

/// GET /v1/tree/{pool}/root?at_ledger=&at_leaf_count=
///
/// Without params, serves the current root from the in-memory tree. With
/// `at_ledger` or `at_leaf_count`, answers from the `tree_roots` history —
/// an index lookup, never a tree rebuild. Provers use this to check their
/// proof's anchor is still inside the contract's accepted-root window;
/// disclosure `verify` uses it to re-check old reports (Issues 4/5).
pub async fn get_root(
    State(state): State<Arc<AppState>>,
    Path(pool): Path<String>,
    Query(q): Query<RootQuery>,
) -> Result<Json<RootResponse>, ApiError> {
    if q.at_ledger.is_some() && q.at_leaf_count.is_some() {
        return Err(ApiError::bad_request(
            "at_ledger and at_leaf_count are mutually exclusive",
        ));
    }

    // Top up (and thus persist history for) the tree first, so historical
    // answers cover everything indexed up to this moment.
    {
        let mut trees = lock_trees(&state).await;
        let pool_tree = sync_pool_tree(&state, &mut trees, &pool).await?;
        if q.at_ledger.is_none() && q.at_leaf_count.is_none() {
            return Ok(Json(RootResponse {
                pool,
                root: hex0x(&pool_tree.tree.root()),
                leaf_count: pool_tree.tree.len() as i64,
                anchored_ledger: pool_tree.anchored_ledger,
            }));
        }
    } // drop the tree lock before the history lookup

    let row: Option<(Vec<u8>, i64, i64)> = if let Some(at_ledger) = q.at_ledger {
        sqlx::query_as(
            "SELECT root, leaf_count, ledger FROM tree_roots
             WHERE pool = $1 AND ledger <= $2
             ORDER BY leaf_count DESC LIMIT 1",
        )
        .bind(&pool)
        .bind(at_ledger)
        .fetch_optional(&state.db)
        .await?
    } else {
        sqlx::query_as(
            "SELECT root, leaf_count, ledger FROM tree_roots
             WHERE pool = $1 AND leaf_count <= $2
             ORDER BY leaf_count DESC LIMIT 1",
        )
        .bind(&pool)
        .bind(q.at_leaf_count.unwrap_or(0))
        .fetch_optional(&state.db)
        .await?
    };

    let (root, leaf_count, ledger) =
        row.ok_or_else(|| ApiError::not_found("no root recorded at or before that point"))?;
    let root: Node = root
        .try_into()
        .map_err(|_| ApiError::bad_request("corrupt root in history"))?;
    Ok(Json(RootResponse {
        pool,
        root: hex0x(&root),
        leaf_count,
        anchored_ledger: ledger,
    }))
}

/// Take the shared tree lock, recording how long the wait took (the lock
/// is the tree endpoints' main contention point — worth watching).
async fn lock_trees(
    state: &AppState,
) -> tokio::sync::MutexGuard<'_, std::collections::HashMap<String, PoolTree>> {
    let started = std::time::Instant::now();
    let guard = state.trees.lock().await;
    metrics::histogram!("attesta_api_tree_lock_wait_seconds")
        .record(started.elapsed().as_secs_f64());
    guard
}

/// Top up the cached tree for `pool` with leaves indexed since the last
/// request, and return it. 404s for a pool with no leaves at all.
async fn sync_pool_tree<'a>(
    state: &AppState,
    trees: &'a mut std::collections::HashMap<String, PoolTree>,
    pool: &str,
) -> Result<&'a PoolTree, ApiError> {
    let topup_started = std::time::Instant::now();
    let pool_tree = trees.entry(pool.to_string()).or_default();

    let rows: Vec<(i64, Vec<u8>, i64)> = sqlx::query_as(
        "SELECT leaf_index, commitment, ledger FROM commitments
         WHERE pool = $1 AND leaf_index >= $2 ORDER BY leaf_index",
    )
    .bind(pool)
    .bind(pool_tree.tree.len() as i64)
    .fetch_all(&state.db)
    .await?;

    // Root history rows accumulated while appending; flushed in one batch
    // below so a large backfill is not one INSERT per leaf.
    let mut new_roots: Vec<(i64, Vec<u8>, i64)> = Vec::new();

    for (leaf_index, bytes, ledger) in rows {
        // Leaves must arrive contiguously; a gap means the indexer is
        // mid-backfill (or missed events). Serve what we have up to it —
        // appending past a gap would put every later leaf at the wrong index.
        if leaf_index != pool_tree.tree.len() as i64 {
            tracing::warn!(
                pool,
                expected = pool_tree.tree.len(),
                got = leaf_index,
                "gap in commitment leaf indexes; serving tree up to the gap"
            );
            break;
        }
        let node: Node = bytes
            .try_into()
            .map_err(|_| ApiError::bad_request("corrupt commitment in index"))?;
        let idx = pool_tree.tree.append(node);
        pool_tree.index_by_commitment.insert(node, idx);
        pool_tree.anchored_ledger = pool_tree.anchored_ledger.max(ledger);
        new_roots.push((
            pool_tree.tree.len() as i64,
            pool_tree.tree.root().to_vec(),
            ledger,
        ));
    }

    // Persist the root-after-each-append history (Issue 5). The tree is
    // append-only, so these rows are deterministic and idempotent: replays
    // after a database drop reproduce identical values, and ON CONFLICT
    // keeps concurrent requests from racing.
    if !new_roots.is_empty() {
        let (leaf_counts, roots, ledgers): (Vec<i64>, Vec<Vec<u8>>, Vec<i64>) = unzip3(new_roots);
        sqlx::query(
            "INSERT INTO tree_roots (pool, leaf_count, root, ledger)
             SELECT $1, * FROM UNNEST($2::bigint[], $3::bytea[], $4::bigint[])
             ON CONFLICT (pool, leaf_count) DO NOTHING",
        )
        .bind(pool)
        .bind(&leaf_counts)
        .bind(&roots)
        .bind(&ledgers)
        .execute(&state.db)
        .await?;
    }

    metrics::histogram!("attesta_api_tree_topup_duration_seconds")
        .record(topup_started.elapsed().as_secs_f64());
    metrics::gauge!("attesta_api_tree_leaves", "pool" => pool.to_string())
        .set(pool_tree.tree.len() as f64);

    if pool_tree.tree.is_empty() {
        return Err(ApiError::not_found(format!(
            "unknown or empty pool: {pool}"
        )));
    }
    Ok(pool_tree)
}

fn unzip3<A, B, C>(v: Vec<(A, B, C)>) -> (Vec<A>, Vec<B>, Vec<C>) {
    let mut a = Vec::with_capacity(v.len());
    let mut b = Vec::with_capacity(v.len());
    let mut c = Vec::with_capacity(v.len());
    for (x, y, z) in v {
        a.push(x);
        b.push(y);
        c.push(z);
    }
    (a, b, c)
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

//! Public protocol stats: pool TVL (public by construction), attestation
//! and issuer counts. Nothing here can reveal a shielded amount.

use std::{sync::Arc, time::Instant};

use attesta_core::models::{PoolStats, ProtocolStats};
use axum::{
    extract::State,
    http::header,
    response::{IntoResponse, Response},
};

use crate::{error::ApiError, state::AppState};

/// GET /v1/stats
///
/// Each of the four counts here is an unbounded full-table scan, and this
/// is a public, unauthenticated, per-IP-rate-limited-only endpoint — cost
/// grows with both table size and distinct visitor count. The assembled
/// result is cached for `STATS_CACHE_TTL_SECS` (default 10s, 0 disables
/// caching) so repeat traffic within the window is free (ISSUES-2.md
/// Issue 17); `Cache-Control` lets a fronting proxy/CDN absorb the rest.
pub async fn get_stats(State(state): State<Arc<AppState>>) -> Result<Response, ApiError> {
    let ttl = state.config.stats_cache_ttl_secs;

    if ttl > 0 {
        if let Some((computed_at, cached)) = state
            .stats_cache
            .lock()
            .expect("stats cache lock poisoned")
            .clone()
        {
            if computed_at.elapsed().as_secs() < ttl as u64 {
                return Ok(with_cache_header(cached, ttl));
            }
        }
    }

    let stats = compute_stats(&state).await?;

    if ttl > 0 {
        *state.stats_cache.lock().expect("stats cache lock poisoned") =
            Some((Instant::now(), stats.clone()));
    }

    Ok(with_cache_header(stats, ttl))
}

async fn compute_stats(state: &AppState) -> Result<ProtocolStats, ApiError> {
    let pools: Vec<PoolStats> = sqlx::query_as(
        "SELECT pool, asset, (total_in - total_out)::text AS tvl
         FROM pool_totals ORDER BY pool",
    )
    .fetch_all(&state.db)
    .await?;

    let total_commitments: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM commitments")
        .fetch_one(&state.db)
        .await?;
    let total_nullifiers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nullifiers")
        .fetch_one(&state.db)
        .await?;
    let active_issuers: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM issuers WHERE status = 'active'")
            .fetch_one(&state.db)
            .await?;
    let credentials_delivered: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM credential_deliveries")
            .fetch_one(&state.db)
            .await?;

    Ok(ProtocolStats {
        pools,
        total_commitments,
        total_nullifiers,
        active_issuers,
        credentials_delivered,
    })
}

fn with_cache_header(stats: ProtocolStats, ttl: u32) -> Response {
    if ttl == 0 {
        return axum::Json(stats).into_response();
    }
    (
        [(header::CACHE_CONTROL, format!("public, max-age={ttl}"))],
        axum::Json(stats),
    )
        .into_response()
}

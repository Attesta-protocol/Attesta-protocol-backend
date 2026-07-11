//! Public protocol stats: pool TVL (public by construction), attestation
//! and issuer counts. Nothing here can reveal a shielded amount.

use std::sync::Arc;

use attesta_core::models::{PoolStats, ProtocolStats};
use axum::{extract::State, Json};

use crate::{error::ApiError, state::AppState};

/// GET /v1/stats
pub async fn get_stats(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ProtocolStats>, ApiError> {
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

    Ok(Json(ProtocolStats {
        pools,
        total_commitments,
        total_nullifiers,
        active_issuers,
        credentials_delivered,
    }))
}

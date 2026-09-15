//! Mailbox retention sweeper (docs/credential-mailbox.md).
//!
//! Deletes claimed deliveries N days after claiming and unclaimed ones M
//! days after delivery, so abandoned mailboxes and wrong-hint deposits do
//! not grow `credential_deliveries` without bound. Either window set to 0
//! disables that deletion — self-hosters may want to keep everything.

use std::{sync::Arc, time::Duration};

use sqlx::postgres::PgConnection;

use crate::state::AppState;

const SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// Bounded deletes so one sweep never holds long row locks.
const SWEEP_BATCH: i64 = 10_000;

/// Arbitrary fixed key for the session-level advisory lock that
/// coordinates the sweeper across API replicas: with N replicas pointed
/// at one database, only the holder actually sweeps each cycle, the rest
/// skip with a debug log (ISSUES-2.md Issue 14). Deletes are idempotent
/// so concurrent sweeping was never unsafe, just duplicated work and
/// interleaved lock contention on every table scan.
const RETENTION_LOCK_KEY: i64 = 0x4154_5445_5354_4152; // ASCII "ATTESTAR"

pub async fn run(state: Arc<AppState>) {
    let claimed_days = state.config.credential_retention_claimed_days;
    let unclaimed_days = state.config.credential_retention_unclaimed_days;
    if claimed_days == 0 && unclaimed_days == 0 {
        tracing::info!("credential retention disabled (both windows are 0)");
        return;
    }

    loop {
        run_cycle(&state, claimed_days, unclaimed_days).await;
        tokio::time::sleep(SWEEP_INTERVAL).await;
    }
}

/// One sweep cycle, holding the advisory lock (and thus a single pooled
/// connection) for its whole duration so the lock covers the actual
/// deletes, not just the acquisition check.
async fn run_cycle(state: &AppState, claimed_days: u32, unclaimed_days: u32) {
    let mut conn = match state.db.acquire().await {
        Ok(conn) => conn,
        Err(e) => {
            tracing::warn!(error = %e, "retention sweep: failed to acquire a connection");
            return;
        }
    };

    let locked: bool = match sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(RETENTION_LOCK_KEY)
        .fetch_one(&mut *conn)
        .await
    {
        Ok(locked) => locked,
        Err(e) => {
            tracing::warn!(error = %e, "retention sweep: advisory lock query failed");
            return;
        }
    };
    if !locked {
        tracing::debug!("retention sweep skipped: another replica holds the lock");
        return;
    }

    if claimed_days > 0 {
        sweep(
            &mut conn,
            "claimed_at IS NOT NULL AND claimed_at < now() - make_interval(days => $1)",
            claimed_days,
            "claimed",
        )
        .await;
    }
    if unclaimed_days > 0 {
        sweep(
            &mut conn,
            "claimed_at IS NULL AND created_at < now() - make_interval(days => $1)",
            unclaimed_days,
            "unclaimed",
        )
        .await;
    }

    if let Err(e) = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(RETENTION_LOCK_KEY)
        .execute(&mut *conn)
        .await
    {
        tracing::warn!(error = %e, "retention sweep: failed to release advisory lock");
    }
}

async fn sweep(conn: &mut PgConnection, predicate: &str, days: u32, kind: &str) {
    loop {
        let sql = format!(
            "DELETE FROM credential_deliveries WHERE delivery_id IN (
                 SELECT delivery_id FROM credential_deliveries
                 WHERE {predicate} LIMIT $2)"
        );
        match sqlx::query(&sql)
            .bind(days as i32)
            .bind(SWEEP_BATCH)
            .execute(&mut *conn)
            .await
        {
            Ok(res) => {
                let n = res.rows_affected();
                if n > 0 {
                    tracing::info!(kind, deleted = n, "retention sweep");
                }
                if (n as i64) < SWEEP_BATCH {
                    return; // drained
                }
            }
            Err(e) => {
                tracing::warn!(kind, error = %e, "retention sweep failed; will retry next cycle");
                return;
            }
        }
    }
}

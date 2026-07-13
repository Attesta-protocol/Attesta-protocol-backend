//! Mailbox retention sweeper (docs/credential-mailbox.md).
//!
//! Deletes claimed deliveries N days after claiming and unclaimed ones M
//! days after delivery, so abandoned mailboxes and wrong-hint deposits do
//! not grow `credential_deliveries` without bound. Either window set to 0
//! disables that deletion — self-hosters may want to keep everything.

use std::{sync::Arc, time::Duration};

use crate::state::AppState;

const SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// Bounded deletes so one sweep never holds long row locks.
const SWEEP_BATCH: i64 = 10_000;

pub async fn run(state: Arc<AppState>) {
    let claimed_days = state.config.credential_retention_claimed_days;
    let unclaimed_days = state.config.credential_retention_unclaimed_days;
    if claimed_days == 0 && unclaimed_days == 0 {
        tracing::info!("credential retention disabled (both windows are 0)");
        return;
    }

    loop {
        if claimed_days > 0 {
            sweep(
                &state,
                "claimed_at IS NOT NULL AND claimed_at < now() - make_interval(days => $1)",
                claimed_days,
                "claimed",
            )
            .await;
        }
        if unclaimed_days > 0 {
            sweep(
                &state,
                "claimed_at IS NULL AND created_at < now() - make_interval(days => $1)",
                unclaimed_days,
                "unclaimed",
            )
            .await;
        }
        tokio::time::sleep(SWEEP_INTERVAL).await;
    }
}

async fn sweep(state: &AppState, predicate: &str, days: u32, kind: &str) {
    loop {
        let sql = format!(
            "DELETE FROM credential_deliveries WHERE delivery_id IN (
                 SELECT delivery_id FROM credential_deliveries
                 WHERE {predicate} LIMIT $2)"
        );
        match sqlx::query(&sql)
            .bind(days as i32)
            .bind(SWEEP_BATCH)
            .execute(&state.db)
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

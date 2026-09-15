use std::time::Duration;

use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::CoreError;

/// Retry budget for the initial connection: bare-metal and k8s deployments
/// (unlike compose, which has `depends_on: service_healthy`) can start the
/// API/indexer before Postgres is accepting connections. Ten attempts,
/// each capped at 5s, with backoff doubling from 500ms to a 10s cap, span
/// under two minutes worst case — covers ordinary cold-start races
/// without hanging indefinitely.
const CONNECT_MAX_ATTEMPTS: u32 = 10;
const CONNECT_INITIAL_BACKOFF: Duration = Duration::from_millis(500);
const CONNECT_MAX_BACKOFF: Duration = Duration::from_secs(10);
/// Per-attempt timeout. sqlx's own default (30s) would let one hung
/// attempt blow the entire retry budget; each attempt failing fast is
/// what makes exponential backoff actually control the cadence.
const CONNECT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);

/// Connect with bounded exponential-backoff retry, so a Postgres that is
/// merely slow to start (compose cold start, node reboot ordering) doesn't
/// take the process down with it. Authentication failures are never
/// retried — a wrong password won't fix itself, and retrying it for a
/// minute only delays a fast, clear failure.
pub async fn connect(database_url: &str) -> Result<PgPool, CoreError> {
    let mut backoff = CONNECT_INITIAL_BACKOFF;
    for attempt in 1..=CONNECT_MAX_ATTEMPTS {
        match PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(CONNECT_ATTEMPT_TIMEOUT)
            .connect(database_url)
            .await
        {
            Ok(pool) => return Ok(pool),
            Err(e) if attempt == CONNECT_MAX_ATTEMPTS || !is_retryable(&e) => {
                return Err(e.into());
            }
            Err(e) => {
                tracing::warn!(
                    attempt,
                    max_attempts = CONNECT_MAX_ATTEMPTS,
                    error = %e,
                    retry_in_secs = backoff.as_secs_f64(),
                    "database connection failed; retrying"
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(CONNECT_MAX_BACKOFF);
            }
        }
    }
    unreachable!("loop always returns on the final attempt")
}

/// Authentication/authorization failures (wrong password, wrong user, no
/// permission on the database) are terminal — no amount of retrying
/// reconnects with the same bad credentials. Everything else (connection
/// refused, timed out, Postgres still starting up) is worth retrying.
fn is_retryable(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::Database(db_err) => !matches!(
            db_err.code().as_deref(),
            Some("28P01") | Some("28000") | Some("3D000")
        ),
        _ => true,
    }
}

/// Run embedded migrations. All indexer state is replayable from chain
/// events, so dropping the database and re-migrating is always safe.
pub async fn migrate(pool: &PgPool) -> Result<(), CoreError> {
    sqlx::migrate!("../../migrations").run(pool).await?;
    Ok(())
}

use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::CoreError;

pub async fn connect(database_url: &str) -> Result<PgPool, CoreError> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await?;
    Ok(pool)
}

/// Run embedded migrations. All indexer state is replayable from chain
/// events, so dropping the database and re-migrating is always safe.
pub async fn migrate(pool: &PgPool) -> Result<(), CoreError> {
    sqlx::migrate!("../../migrations").run(pool).await?;
    Ok(())
}

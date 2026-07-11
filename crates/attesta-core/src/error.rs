use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),

    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    #[error("unknown pool: {0}")]
    UnknownPool(String),

    #[error("commitment not found in tree")]
    CommitmentNotFound,

    #[error("invalid input: {0}")]
    InvalidInput(String),
}

use attesta_core::{config::Config, models::EncryptedNoteRow};
use sqlx::PgPool;
use tokio::sync::broadcast;

pub struct AppState {
    pub db: PgPool,
    pub config: Config,
    /// New encrypted notes are broadcast here for SSE subscribers.
    pub note_tx: broadcast::Sender<EncryptedNoteRow>,
}

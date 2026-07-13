//! Encrypted-note relay. Serves ciphertext blobs; recipients trial-decrypt
//! client-side with their viewing keys. This service cannot read a note.

use std::{convert::Infallible, net::SocketAddr, sync::Arc, time::Duration};

use attesta_core::models::EncryptedNoteRow;
use axum::{
    extract::{ConnectInfo, Query, State},
    response::{
        sse::{Event, KeepAlive, Sse},
        Response,
    },
    Json,
};
use futures::Stream;
use serde::{Deserialize, Serialize};
use tokio_stream::{wrappers::BroadcastStream, StreamExt};

use crate::{error::ApiError, limits::too_many_requests, state::AppState};

const PAGE_SIZE: i64 = 500;

#[derive(Deserialize)]
pub struct NotesQuery {
    /// Resume cursor from a previous page (exclusive).
    pub since_cursor: Option<i64>,
    /// Optional pool filter.
    pub pool: Option<String>,
}

#[derive(Serialize)]
pub struct NotesPage {
    pub notes: Vec<EncryptedNoteRow>,
    /// Pass as since_cursor to fetch the next page. Absent on the last page.
    pub next_cursor: Option<i64>,
}

/// GET /v1/notes?since_cursor=&pool=
pub async fn list_notes(
    State(state): State<Arc<AppState>>,
    Query(q): Query<NotesQuery>,
) -> Result<Json<NotesPage>, ApiError> {
    let since = q.since_cursor.unwrap_or(0);
    let notes: Vec<EncryptedNoteRow> = sqlx::query_as(
        "SELECT id, pool, commitment, ephemeral_pubkey, ciphertext, ledger, tx_hash
         FROM encrypted_notes
         WHERE id > $1 AND ($2::text IS NULL OR pool = $2)
         ORDER BY id
         LIMIT $3",
    )
    .bind(since)
    .bind(q.pool.as_deref())
    .bind(PAGE_SIZE)
    .fetch_all(&state.db)
    .await?;

    let next_cursor = if notes.len() as i64 == PAGE_SIZE {
        notes.last().map(|n| n.id)
    } else {
        None
    };

    Ok(Json(NotesPage { notes, next_cursor }))
}

/// GET /v1/notes/stream — SSE stream of newly indexed encrypted notes.
///
/// Concurrent connections are capped per IP and globally (429 +
/// Retry-After when exhausted); each connection holds one RAII slot that
/// is released when the stream drops, so one client cannot starve other
/// subscribers.
pub async fn stream_notes(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Response> {
    let Some(slot) = state.sse_slots.try_acquire(addr.ip()) else {
        return Err(too_many_requests(30));
    };

    let rx = state.note_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(move |msg| {
        // The closure owns the slot; it drops when the stream does.
        let _held = &slot;
        // Slow subscribers that miss broadcasts just re-sync via /v1/notes.
        let note = msg.ok()?;
        Some(Ok(note_event(&note)?))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(20))))
}

/// SSE event for one note. The `id:` field carries the note's monotonic
/// cursor, so standard `Last-Event-ID` reconnects can resume losslessly.
fn note_event(note: &EncryptedNoteRow) -> Option<Event> {
    Event::default()
        .event("note")
        .id(note.id.to_string())
        .json_data(note)
        .ok()
}

/// Background task: watch the encrypted_notes table and broadcast new rows
/// to SSE subscribers. DB polling keeps the API and indexer fully decoupled
/// (they can run as separate processes/containers).
pub async fn poll_new_notes(state: Arc<AppState>) {
    let mut last_id: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM encrypted_notes")
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);

    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        if state.note_tx.receiver_count() == 0 {
            continue;
        }
        let rows: Result<Vec<EncryptedNoteRow>, _> = sqlx::query_as(
            "SELECT id, pool, commitment, ephemeral_pubkey, ciphertext, ledger, tx_hash
             FROM encrypted_notes WHERE id > $1 ORDER BY id LIMIT 1000",
        )
        .bind(last_id)
        .fetch_all(&state.db)
        .await;

        match rows {
            Ok(rows) => {
                for note in rows {
                    last_id = note.id;
                    let _ = state.note_tx.send(note);
                }
            }
            Err(e) => tracing::warn!(error = %e, "note poller query failed"),
        }
    }
}

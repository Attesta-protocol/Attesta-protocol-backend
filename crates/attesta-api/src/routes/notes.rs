//! Encrypted-note relay. Serves ciphertext blobs; recipients trial-decrypt
//! client-side with their viewing keys. This service cannot read a note.

use std::{convert::Infallible, net::SocketAddr, sync::Arc, time::Duration};

use attesta_core::models::EncryptedNoteRow;
use axum::{
    extract::{ConnectInfo, Query, State},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
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

/// Cap on rows replayed inline on reconnect; beyond this the client is
/// told to re-page via /v1/notes with a `resync` event.
const REPLAY_LIMIT: i64 = 5_000;

#[derive(Deserialize)]
pub struct StreamQuery {
    /// Equivalent to the Last-Event-ID header, for clients (e.g. curl or
    /// EventSource polyfills) that cannot set it.
    pub since_cursor: Option<i64>,
}

/// GET /v1/notes/stream — SSE stream of newly indexed encrypted notes.
///
/// Resumable: events carry `id:`, and a reconnect with `Last-Event-ID: N`
/// (or `?since_cursor=N`) first replays every stored note with `id > N`
/// in order, then continues live — no gaps, no duplicates (the live
/// broadcast is subscribed *before* the replay query, and overlap is
/// deduped by cursor). If more than REPLAY_LIMIT rows are pending, a
/// `resync` event tells the client to re-page via /v1/notes instead.
///
/// Concurrent connections are capped per IP and globally (429 +
/// Retry-After when exhausted); each connection holds one RAII slot that
/// is released when the stream drops, so one client cannot starve other
/// subscribers.
pub async fn stream_notes(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(q): Query<StreamQuery>,
    headers: axum::http::HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Response> {
    let Some(slot) = state.sse_slots.try_acquire(addr.ip()) else {
        return Err(too_many_requests(30));
    };

    let resume_from = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .or(q.since_cursor);

    // Subscribe before the replay query so nothing inserted in between is
    // lost; the overlap is deduped by cursor below.
    let rx = state.note_tx.subscribe();

    let (replay, mut last_seen) = match resume_from {
        Some(cursor) => {
            let rows: Vec<EncryptedNoteRow> = sqlx::query_as(
                "SELECT id, pool, commitment, ephemeral_pubkey, ciphertext, ledger, tx_hash
                 FROM encrypted_notes WHERE id > $1 ORDER BY id LIMIT $2",
            )
            .bind(cursor)
            .bind(REPLAY_LIMIT)
            .fetch_all(&state.db)
            .await
            .map_err(|e| ApiError::from(e).into_response())?;

            let overflow = rows.len() as i64 == REPLAY_LIMIT;
            let last = rows.last().map(|n| n.id).unwrap_or(cursor);
            let mut events: Vec<Result<Event, Infallible>> =
                rows.iter().filter_map(note_event).map(Ok).collect();
            if overflow {
                events.push(Ok(resync_event()));
            }
            (events, last)
        }
        None => (Vec::new(), 0),
    };

    let live = BroadcastStream::new(rx).filter_map(move |msg| {
        // The closure owns the slot; it drops when the stream does.
        let _held = &slot;
        match msg {
            Ok(note) => {
                // Dedup the replay/live overlap: cursors are monotonic.
                if note.id <= last_seen {
                    return None;
                }
                last_seen = note.id;
                Some(Ok(note_event(&note)?))
            }
            // Slow consumer overflowed the broadcast buffer: tell it to
            // re-page via /v1/notes instead of silently missing notes.
            Err(_lagged) => Some(Ok(resync_event())),
        }
    });

    let stream = futures::stream::iter(replay).chain(live);
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(20))))
}

/// Tells well-behaved clients to re-page via /v1/notes from their last
/// processed cursor before trusting the live stream again.
fn resync_event() -> Event {
    Event::default().event("resync").data("re-page /v1/notes")
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

/// Poll cadence when LISTEN is unavailable, and the safety-net re-check
/// interval while it is (covers dropped notifications).
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const LISTEN_SAFETY_INTERVAL: Duration = Duration::from_secs(10);

/// Background task: broadcast newly indexed encrypted notes to SSE
/// subscribers. Prefers Postgres LISTEN/NOTIFY (push latency, no idle
/// queries; the API and indexer still share only the database), degrading
/// to the 2 s table poll whenever the LISTEN connection is unavailable —
/// the stream never goes down with it. Notifications are treated purely
/// as wake-ups: rows are always read by cursor, so coalesced or dropped
/// notifications cannot skip notes.
pub async fn poll_new_notes(state: Arc<AppState>) {
    let mut last_id: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM encrypted_notes")
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);

    loop {
        match listen(&state, &mut last_id).await {
            Ok(()) => unreachable!("listen loop only exits with an error"),
            Err(e) => {
                tracing::warn!(error = %e, "LISTEN unavailable; falling back to polling");
            }
        }
        // Poll for a while, then try to re-establish LISTEN.
        for _ in 0..15 {
            tokio::time::sleep(POLL_INTERVAL).await;
            drain_new_notes(&state, &mut last_id).await;
        }
    }
}

/// Push mode: wake on NOTIFY (or the safety interval) and drain.
async fn listen(state: &Arc<AppState>, last_id: &mut i64) -> Result<(), sqlx::Error> {
    let mut listener = sqlx::postgres::PgListener::connect_with(&state.db).await?;
    listener.listen("attesta_notes").await?;
    tracing::info!("note fan-out in push mode (LISTEN attesta_notes)");

    loop {
        // Drain first: covers rows inserted before LISTEN was set up and
        // any notifications lost while reconnecting.
        drain_new_notes(state, last_id).await;
        match tokio::time::timeout(LISTEN_SAFETY_INTERVAL, listener.recv()).await {
            Ok(Ok(_notification)) => {} // wake → drain on next iteration
            Ok(Err(e)) => return Err(e),
            Err(_elapsed) => {} // safety-net poll
        }
    }
}

async fn drain_new_notes(state: &Arc<AppState>, last_id: &mut i64) {
    loop {
        let rows: Result<Vec<EncryptedNoteRow>, _> = sqlx::query_as(
            "SELECT id, pool, commitment, ephemeral_pubkey, ciphertext, ledger, tx_hash
             FROM encrypted_notes WHERE id > $1 ORDER BY id LIMIT 1000",
        )
        .bind(*last_id)
        .fetch_all(&state.db)
        .await;

        match rows {
            Ok(rows) => {
                let full_page = rows.len() == 1000;
                for note in rows {
                    *last_id = note.id;
                    let _ = state.note_tx.send(note);
                }
                if !full_page {
                    return;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "note fan-out query failed");
                return;
            }
        }
    }
}

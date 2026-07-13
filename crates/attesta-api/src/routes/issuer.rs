//! Issuer gateway: credential delivery mailbox + issuer registry mirror.
//!
//! Credentials arrive here already encrypted to the recipient. The gateway
//! never sees claim contents — only ciphertext, an issuer id, and an opaque
//! recipient-derived mailbox tag. There is deliberately no field in the
//! request schema where plaintext credential data could go.

use std::sync::Arc;

use attesta_core::models::{CredentialDeliveryRow, IssuerRow};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{error::ApiError, state::AppState};

const MAX_CIPHERTEXT_BYTES: usize = 64 * 1024;

#[derive(Deserialize)]
pub struct DeliverCredentialRequest {
    pub issuer_id: String,
    /// Opaque mailbox tag derived by the recipient (e.g. a hash of their
    /// collection key). Not an identity.
    pub recipient_hint: String,
    /// Base64 credential ciphertext, encrypted to the recipient off-chain.
    pub ciphertext: String,
    /// Base64 issuer signature over the ciphertext.
    pub issuer_signature: String,
    /// Base64 SHA-256 of a claim token carried inside the encrypted
    /// payload. Optional; without it the delivery can never be claimed
    /// and only ages out via retention (docs/credential-mailbox.md).
    pub claim_token_hash: Option<String>,
}

#[derive(Serialize)]
pub struct DeliverCredentialResponse {
    pub delivery_id: Uuid,
}

/// POST /v1/issuer/credentials
pub async fn deliver_credential(
    State(state): State<Arc<AppState>>,
    Json(req): Json<DeliverCredentialRequest>,
) -> Result<(StatusCode, Json<DeliverCredentialResponse>), ApiError> {
    let ciphertext = B64
        .decode(&req.ciphertext)
        .map_err(|_| ApiError::bad_request("ciphertext must be base64"))?;
    if ciphertext.is_empty() || ciphertext.len() > MAX_CIPHERTEXT_BYTES {
        return Err(ApiError::bad_request("ciphertext size out of bounds"));
    }
    let signature = B64
        .decode(&req.issuer_signature)
        .map_err(|_| ApiError::bad_request("issuer_signature must be base64"))?;
    if req.recipient_hint.is_empty() || req.recipient_hint.len() > 128 {
        return Err(ApiError::bad_request("recipient_hint size out of bounds"));
    }
    let claim_token_hash = req
        .claim_token_hash
        .as_deref()
        .map(|s| {
            let hash = B64
                .decode(s)
                .map_err(|_| ApiError::bad_request("claim_token_hash must be base64"))?;
            if hash.len() != 32 {
                return Err(ApiError::bad_request("claim_token_hash must be 32 bytes"));
            }
            Ok(hash)
        })
        .transpose()?;

    let issuer: Option<IssuerRow> = sqlx::query_as(
        "SELECT issuer_id, name, public_key, claim_types, status, registered_ledger
         FROM issuers WHERE issuer_id = $1 AND status = 'active'",
    )
    .bind(&req.issuer_id)
    .fetch_optional(&state.db)
    .await?;
    let issuer = issuer.ok_or_else(|| ApiError::bad_request("unknown or inactive issuer"))?;

    // TODO(M5): verify `signature` against issuer.public_key over the
    // ciphertext before accepting. Requires the credential envelope format
    // to be finalized (see docs/credential-format.md when it lands).
    let _ = &issuer.public_key;

    // Per-issuer hourly quota (0 = unlimited): one compromised or buggy
    // issuer key cannot flood the mailbox table. Sliding window over the
    // rows themselves — no extra state to keep consistent.
    let quota = state.config.rate_limits.issuer_deliveries_per_hour;
    if quota > 0 {
        let delivered: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM credential_deliveries
             WHERE issuer_id = $1 AND created_at > now() - interval '1 hour'",
        )
        .bind(&req.issuer_id)
        .fetch_one(&state.db)
        .await?;
        if delivered >= quota as i64 {
            return Err(ApiError::too_many_requests(
                "issuer hourly delivery quota exceeded",
            ));
        }
    }

    let delivery_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO credential_deliveries
             (delivery_id, issuer_id, recipient_hint, ciphertext, issuer_signature,
              claim_token_hash)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(delivery_id)
    .bind(&req.issuer_id)
    .bind(&req.recipient_hint)
    .bind(&ciphertext)
    .bind(&signature)
    .bind(&claim_token_hash)
    .execute(&state.db)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(DeliverCredentialResponse { delivery_id }),
    ))
}

#[derive(Deserialize)]
pub struct ClaimRequest {
    /// Base64 claim token recovered from inside the decrypted payload.
    pub claim_token: String,
}

/// POST /v1/credentials/{delivery_id}/claim
///
/// Marks a delivery claimed so it drops out of pickup results. The caller
/// proves they are the true recipient by presenting the preimage of the
/// `claim_token_hash` the issuer stored at delivery time — the token
/// travels inside the encrypted payload, so producing it is exactly as
/// hard as breaking the encryption (docs/credential-mailbox.md).
pub async fn claim_delivery(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(delivery_id): axum::extract::Path<Uuid>,
    Json(req): Json<ClaimRequest>,
) -> Result<StatusCode, ApiError> {
    let token = B64
        .decode(&req.claim_token)
        .map_err(|_| ApiError::bad_request("claim_token must be base64"))?;
    if token.is_empty() || token.len() > 128 {
        return Err(ApiError::bad_request("claim_token size out of bounds"));
    }
    let presented_hash: [u8; 32] = Sha256::digest(&token).into();

    type ClaimStateRow = (Option<Vec<u8>>, Option<DateTime<Utc>>);
    let row: Option<ClaimStateRow> = sqlx::query_as(
        "SELECT claim_token_hash, claimed_at FROM credential_deliveries
         WHERE delivery_id = $1",
    )
    .bind(delivery_id)
    .fetch_optional(&state.db)
    .await?;
    let (stored_hash, claimed_at) = row.ok_or_else(|| ApiError::not_found("unknown delivery"))?;

    if claimed_at.is_some() {
        return Err(ApiError::conflict("delivery already claimed"));
    }
    // Constant-time equality is unnecessary here: the compared values are
    // hashes, and a mismatch reveals nothing about the stored preimage.
    let authorized = stored_hash
        .as_deref()
        .is_some_and(|h| h == presented_hash.as_slice());
    if !authorized {
        return Err(ApiError::forbidden("claim token does not match"));
    }

    // Guard claimed_at IS NULL again in the UPDATE so a concurrent claim
    // race resolves to exactly one winner.
    let updated = sqlx::query(
        "UPDATE credential_deliveries SET claimed_at = now()
         WHERE delivery_id = $1 AND claimed_at IS NULL",
    )
    .bind(delivery_id)
    .execute(&state.db)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::conflict("delivery already claimed"));
    }
    Ok(StatusCode::NO_CONTENT)
}

const DELIVERIES_PAGE_SIZE: i64 = 200;

#[derive(Deserialize)]
pub struct ListDeliveriesQuery {
    pub recipient_hint: String,
    /// Resume cursor from a previous page (exclusive), same contract as
    /// /v1/notes.
    pub since_cursor: Option<i64>,
}

#[derive(Serialize)]
pub struct DeliveriesPage {
    pub deliveries: Vec<CredentialDeliveryRow>,
    /// Pass as since_cursor to fetch the next page. Absent on the last page.
    pub next_cursor: Option<i64>,
}

/// GET /v1/credentials?recipient_hint=&since_cursor= — recipient-side
/// pickup of encrypted credential blobs, paginated by the monotonic `seq`
/// column. Pickup is idempotent and read-only; deliveries leave the
/// mailbox only via an authorized claim (see `claim_delivery`) or
/// retention. Decryption (and thus access control) is client-side: a
/// wrong recipient fetches undecryptable ciphertext.
pub async fn list_deliveries(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListDeliveriesQuery>,
) -> Result<Json<DeliveriesPage>, ApiError> {
    let since = q.since_cursor.unwrap_or(0);
    let deliveries: Vec<CredentialDeliveryRow> = sqlx::query_as(
        "SELECT seq, delivery_id, issuer_id, recipient_hint, ciphertext,
                issuer_signature, created_at
         FROM credential_deliveries
         WHERE recipient_hint = $1 AND claimed_at IS NULL AND seq > $2
         ORDER BY seq
         LIMIT $3",
    )
    .bind(&q.recipient_hint)
    .bind(since)
    .bind(DELIVERIES_PAGE_SIZE)
    .fetch_all(&state.db)
    .await?;

    let next_cursor = if deliveries.len() as i64 == DELIVERIES_PAGE_SIZE {
        deliveries.last().map(|d| d.seq)
    } else {
        None
    };
    Ok(Json(DeliveriesPage {
        deliveries,
        next_cursor,
    }))
}

/// GET /v1/issuers — active issuer registry mirror.
pub async fn list_issuers(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<IssuerRow>>, ApiError> {
    let rows: Vec<IssuerRow> = sqlx::query_as(
        "SELECT issuer_id, name, public_key, claim_types, status, registered_ledger
         FROM issuers WHERE status <> 'revoked' ORDER BY issuer_id",
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

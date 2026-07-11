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
use serde::{Deserialize, Serialize};
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

    let delivery_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO credential_deliveries
             (delivery_id, issuer_id, recipient_hint, ciphertext, issuer_signature)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(delivery_id)
    .bind(&req.issuer_id)
    .bind(&req.recipient_hint)
    .bind(&ciphertext)
    .bind(&signature)
    .execute(&state.db)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(DeliverCredentialResponse { delivery_id }),
    ))
}

#[derive(Deserialize)]
pub struct ListDeliveriesQuery {
    pub recipient_hint: String,
}

/// GET /v1/credentials?recipient_hint= — recipient-side pickup of encrypted
/// credential blobs. Decryption (and thus access control) is client-side:
/// a wrong recipient fetches undecryptable ciphertext.
pub async fn list_deliveries(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListDeliveriesQuery>,
) -> Result<Json<Vec<CredentialDeliveryRow>>, ApiError> {
    let rows: Vec<CredentialDeliveryRow> = sqlx::query_as(
        "SELECT delivery_id, issuer_id, recipient_hint, ciphertext, issuer_signature, created_at
         FROM credential_deliveries
         WHERE recipient_hint = $1 AND claimed_at IS NULL
         ORDER BY created_at",
    )
    .bind(&q.recipient_hint)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
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

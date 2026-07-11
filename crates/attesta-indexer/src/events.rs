//! Decoding of Attesta contract events into typed pool/registry events.
//!
//! The shielded-pool and registry contracts (M1/M2) are not deployed yet,
//! so the exact XDR topic/value layout is still settling. This module
//! isolates that decoding: `decode` inspects the event's first topic
//! (the event name symbol) and maps the payload into a [`PoolEvent`].
//!
//! Until the contracts land, unknown events are skipped with a debug log —
//! the ingest loop and storage layer are already final.

use serde::Deserialize;

use crate::rpc::RawEvent;

#[derive(Debug)]
pub enum PoolEvent {
    /// A new commitment appended to the pool's tree (deposit or transfer
    /// output). Deposits carry a public amount for TVL accounting.
    NewCommitment {
        commitment: [u8; 32],
        leaf_index: i64,
        /// Public deposit amount in stroop-scale units (None for transfer
        /// outputs, whose amounts are shielded).
        deposit_amount: Option<i128>,
        asset: Option<String>,
    },
    /// A nullifier revealed by a transfer or withdrawal.
    NullifierSpent { nullifier: [u8; 32] },
    /// Encrypted note blob for the recipient to trial-decrypt.
    EncryptedNote {
        commitment: [u8; 32],
        ephemeral_pubkey: Vec<u8>,
        ciphertext: Vec<u8>,
    },
    /// Public withdrawal amount leaving the pool (TVL accounting).
    Withdrawal { amount: i128, asset: String },
    /// Issuer registry change.
    IssuerUpdated {
        issuer_id: String,
        name: String,
        public_key: Vec<u8>,
        claim_types: Vec<String>,
        status: String,
    },
}

/// Decode a raw Soroban event into a typed event, or None if it is not an
/// Attesta event (or the format is not yet recognized).
pub fn decode(raw: &RawEvent) -> Option<PoolEvent> {
    let name = event_name(raw)?;
    match name.as_str() {
        // TODO(M2): replace the JSON-shim decoding below with real XDR
        // (ScVal) decoding via stellar-xdr once the pool contract's event
        // layout is frozen. The shim lets integration tests drive the full
        // ingest path today by publishing JSON payloads in `value`.
        "new_commitment" => {
            let v: NewCommitmentShim = decode_shim(raw)?;
            Some(PoolEvent::NewCommitment {
                commitment: hex32(&v.commitment)?,
                leaf_index: v.leaf_index,
                deposit_amount: v.deposit_amount.as_deref().and_then(|s| s.parse().ok()),
                asset: v.asset,
            })
        }
        "nullifier" => {
            let v: NullifierShim = decode_shim(raw)?;
            Some(PoolEvent::NullifierSpent {
                nullifier: hex32(&v.nullifier)?,
            })
        }
        "note" => {
            let v: NoteShim = decode_shim(raw)?;
            Some(PoolEvent::EncryptedNote {
                commitment: hex32(&v.commitment)?,
                ephemeral_pubkey: hex_bytes(&v.ephemeral_pubkey)?,
                ciphertext: hex_bytes(&v.ciphertext)?,
            })
        }
        "withdrawal" => {
            let v: WithdrawalShim = decode_shim(raw)?;
            Some(PoolEvent::Withdrawal {
                amount: v.amount.parse().ok()?,
                asset: v.asset,
            })
        }
        "issuer" => {
            let v: IssuerShim = decode_shim(raw)?;
            Some(PoolEvent::IssuerUpdated {
                issuer_id: v.issuer_id,
                name: v.name,
                public_key: hex_bytes(&v.public_key)?,
                claim_types: v.claim_types,
                status: v.status,
            })
        }
        other => {
            tracing::debug!(event = other, id = %raw.id, "skipping unrecognized event");
            None
        }
    }
}

/// First topic = event name. Accepts either a plain string (test shim) or
/// base64 XDR symbol (TODO(M2): proper ScVal decode).
fn event_name(raw: &RawEvent) -> Option<String> {
    let first = raw.topic.first()?;
    if first.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
        return Some(first.clone());
    }
    // Base64 XDR ScSymbol: skip until real XDR decoding lands.
    None
}

fn decode_shim<T: for<'de> Deserialize<'de>>(raw: &RawEvent) -> Option<T> {
    serde_json::from_str(&raw.value).ok()
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    let bytes = hex_bytes(s)?;
    bytes.try_into().ok()
}

fn hex_bytes(s: &str) -> Option<Vec<u8>> {
    hex::decode(s.trim_start_matches("0x")).ok()
}

#[derive(Deserialize)]
struct NewCommitmentShim {
    commitment: String,
    leaf_index: i64,
    deposit_amount: Option<String>,
    asset: Option<String>,
}

#[derive(Deserialize)]
struct NullifierShim {
    nullifier: String,
}

#[derive(Deserialize)]
struct NoteShim {
    commitment: String,
    ephemeral_pubkey: String,
    ciphertext: String,
}

#[derive(Deserialize)]
struct WithdrawalShim {
    amount: String,
    asset: String,
}

#[derive(Deserialize)]
struct IssuerShim {
    issuer_id: String,
    name: String,
    public_key: String,
    claim_types: Vec<String>,
    status: String,
}

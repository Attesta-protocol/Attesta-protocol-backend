use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// A commitment-tree leaf mirrored from chain events.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct CommitmentRow {
    pub pool: String,
    pub leaf_index: i64,
    #[serde(with = "hex_bytes")]
    pub commitment: Vec<u8>,
    pub ledger: i64,
    pub tx_hash: String,
}

/// A spent-note nullifier mirrored from chain events.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct NullifierRow {
    pub pool: String,
    #[serde(with = "hex_bytes")]
    pub nullifier: Vec<u8>,
    pub ledger: i64,
    pub tx_hash: String,
}

/// An encrypted note blob. Ciphertext only — the relay cannot decrypt it.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct EncryptedNoteRow {
    /// Monotonic id, used as the pagination cursor.
    pub id: i64,
    pub pool: String,
    #[serde(with = "hex_bytes")]
    pub commitment: Vec<u8>,
    #[serde(with = "hex_bytes")]
    pub ephemeral_pubkey: Vec<u8>,
    #[serde(with = "hex_bytes")]
    pub ciphertext: Vec<u8>,
    pub ledger: i64,
    pub tx_hash: String,
}

/// Issuer registry mirror entry (public on-chain state).
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct IssuerRow {
    pub issuer_id: String,
    pub name: String,
    #[serde(with = "hex_bytes")]
    pub public_key: Vec<u8>,
    pub claim_types: Vec<String>,
    pub status: String,
    pub registered_ledger: i64,
}

/// An encrypted credential waiting for its recipient to collect it.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct CredentialDeliveryRow {
    pub delivery_id: Uuid,
    pub issuer_id: String,
    pub recipient_hint: String,
    #[serde(with = "hex_bytes")]
    pub ciphertext: Vec<u8>,
    #[serde(with = "hex_bytes")]
    pub issuer_signature: Vec<u8>,
    pub created_at: DateTime<Utc>,
}

/// Public protocol statistics (everything here is public by construction).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolStats {
    pub pools: Vec<PoolStats>,
    pub total_commitments: i64,
    pub total_nullifiers: i64,
    pub active_issuers: i64,
    pub credentials_delivered: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct PoolStats {
    pub pool: String,
    pub asset: String,
    /// TVL as a decimal string: deposits minus withdrawals, both of which
    /// cross the shielded boundary with public amounts.
    pub tvl: String,
}

/// Serde helper: byte columns render as 0x-hex in JSON.
pub mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("0x{}", hex::encode(bytes)))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        hex::decode(s.trim_start_matches("0x")).map_err(serde::de::Error::custom)
    }
}

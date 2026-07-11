//! Decoding of Attesta contract events into typed pool/registry events.
//!
//! Events arrive from Soroban RPC with base64-XDR `ScVal` topics and value.
//! `decode` reads the event name from the first topic (an `ScSymbol`) and
//! maps the payload into a [`PoolEvent`].
//!
//! ## Provisional event layout
//!
//! The shielded-pool and registry contracts (M1/M2) are not deployed yet.
//! Until their event layout freezes, this module decodes the layout below;
//! if the contracts diverge, only the field tables here need updating.
//!
//! - topic[0]: `Symbol` event name (`new_commitment`, `nullifier`, `note`,
//!   `withdrawal`, `issuer`)
//! - value: a `Map` keyed by `Symbol`s matching the field names of the
//!   corresponding [`PoolEvent`] variant. `nullifier` may alternatively
//!   publish its 32 bytes directly as a bare `Bytes` value.
//!
//! A JSON shim is kept as a fallback so integration tests can drive the
//! ingest path with plain-text events; real RPC payloads never hit it.

use serde::Deserialize;
use stellar_xdr::{Limits, ReadXdr, ScMap, ScVal};

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
    let decoded = match name.as_str() {
        "new_commitment" => decode_new_commitment(raw),
        "nullifier" => decode_nullifier(raw),
        "note" => decode_note(raw),
        "withdrawal" => decode_withdrawal(raw),
        "issuer" => decode_issuer(raw),
        other => {
            tracing::debug!(event = other, id = %raw.id, "skipping unrecognized event");
            return None;
        }
    };
    if decoded.is_none() {
        tracing::warn!(event = %name, id = %raw.id, "recognized event failed to decode");
    }
    decoded
}

/// First topic = event name: an XDR `ScSymbol`, or a plain string from the
/// JSON test shim.
fn event_name(raw: &RawEvent) -> Option<String> {
    let first = raw.topic.first()?;
    if let Some(ScVal::Symbol(sym)) = scval_b64(first) {
        return Some(sym.0.to_string());
    }
    if first.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
        return Some(first.clone());
    }
    None
}

fn decode_new_commitment(raw: &RawEvent) -> Option<PoolEvent> {
    if let Some(map) = value_map(raw) {
        return Some(PoolEvent::NewCommitment {
            commitment: bytes32(map_get(&map, "commitment")?)?,
            leaf_index: int64(map_get(&map, "leaf_index")?)?,
            deposit_amount: map_get(&map, "deposit_amount").and_then(int128),
            asset: map_get(&map, "asset").and_then(text),
        });
    }
    let v: NewCommitmentShim = decode_shim(raw)?;
    Some(PoolEvent::NewCommitment {
        commitment: hex32(&v.commitment)?,
        leaf_index: v.leaf_index,
        deposit_amount: v.deposit_amount.as_deref().and_then(|s| s.parse().ok()),
        asset: v.asset,
    })
}

fn decode_nullifier(raw: &RawEvent) -> Option<PoolEvent> {
    match scval_b64(&raw.value) {
        // Single-field event: contracts may publish the bytes directly.
        Some(ScVal::Bytes(b)) => {
            return Some(PoolEvent::NullifierSpent {
                nullifier: b.0.as_slice().try_into().ok()?,
            });
        }
        Some(ScVal::Map(Some(map))) => {
            return Some(PoolEvent::NullifierSpent {
                nullifier: bytes32(map_get(&map, "nullifier")?)?,
            });
        }
        _ => {}
    }
    let v: NullifierShim = decode_shim(raw)?;
    Some(PoolEvent::NullifierSpent {
        nullifier: hex32(&v.nullifier)?,
    })
}

fn decode_note(raw: &RawEvent) -> Option<PoolEvent> {
    if let Some(map) = value_map(raw) {
        return Some(PoolEvent::EncryptedNote {
            commitment: bytes32(map_get(&map, "commitment")?)?,
            ephemeral_pubkey: bytes(map_get(&map, "ephemeral_pubkey")?)?,
            ciphertext: bytes(map_get(&map, "ciphertext")?)?,
        });
    }
    let v: NoteShim = decode_shim(raw)?;
    Some(PoolEvent::EncryptedNote {
        commitment: hex32(&v.commitment)?,
        ephemeral_pubkey: hex_bytes(&v.ephemeral_pubkey)?,
        ciphertext: hex_bytes(&v.ciphertext)?,
    })
}

fn decode_withdrawal(raw: &RawEvent) -> Option<PoolEvent> {
    if let Some(map) = value_map(raw) {
        return Some(PoolEvent::Withdrawal {
            amount: int128(map_get(&map, "amount")?)?,
            asset: text(map_get(&map, "asset")?)?,
        });
    }
    let v: WithdrawalShim = decode_shim(raw)?;
    Some(PoolEvent::Withdrawal {
        amount: v.amount.parse().ok()?,
        asset: v.asset,
    })
}

fn decode_issuer(raw: &RawEvent) -> Option<PoolEvent> {
    if let Some(map) = value_map(raw) {
        return Some(PoolEvent::IssuerUpdated {
            issuer_id: text(map_get(&map, "issuer_id")?)?,
            name: text(map_get(&map, "name")?)?,
            public_key: bytes(map_get(&map, "public_key")?)?,
            claim_types: text_vec(map_get(&map, "claim_types")?)?,
            status: text(map_get(&map, "status")?)?,
        });
    }
    let v: IssuerShim = decode_shim(raw)?;
    Some(PoolEvent::IssuerUpdated {
        issuer_id: v.issuer_id,
        name: v.name,
        public_key: hex_bytes(&v.public_key)?,
        claim_types: v.claim_types,
        status: v.status,
    })
}

// ---- XDR helpers ----

fn scval_b64(s: &str) -> Option<ScVal> {
    ScVal::from_xdr_base64(s, Limits::none()).ok()
}

fn value_map(raw: &RawEvent) -> Option<ScMap> {
    match scval_b64(&raw.value)? {
        ScVal::Map(Some(map)) => Some(map),
        _ => None,
    }
}

fn map_get<'a>(map: &'a ScMap, key: &str) -> Option<&'a ScVal> {
    map.0.iter().find_map(|entry| match &entry.key {
        ScVal::Symbol(sym) if sym.0.to_string() == key => Some(&entry.val),
        _ => None,
    })
}

fn bytes(v: &ScVal) -> Option<Vec<u8>> {
    match v {
        ScVal::Bytes(b) => Some(b.0.to_vec()),
        _ => None,
    }
}

fn bytes32(v: &ScVal) -> Option<[u8; 32]> {
    bytes(v)?.try_into().ok()
}

fn int64(v: &ScVal) -> Option<i64> {
    match v {
        ScVal::U32(n) => Some(i64::from(*n)),
        ScVal::U64(n) => i64::try_from(*n).ok(),
        ScVal::I64(n) => Some(*n),
        _ => None,
    }
}

fn int128(v: &ScVal) -> Option<i128> {
    match v {
        ScVal::I128(parts) => Some((i128::from(parts.hi) << 64) | i128::from(parts.lo)),
        ScVal::U64(n) => Some(i128::from(*n)),
        ScVal::I64(n) => Some(i128::from(*n)),
        ScVal::U32(n) => Some(i128::from(*n)),
        _ => None,
    }
}

fn text(v: &ScVal) -> Option<String> {
    match v {
        ScVal::String(s) => Some(s.0.to_string()),
        ScVal::Symbol(s) => Some(s.0.to_string()),
        _ => None,
    }
}

fn text_vec(v: &ScVal) -> Option<Vec<String>> {
    match v {
        ScVal::Vec(Some(items)) => items.iter().map(text).collect(),
        _ => None,
    }
}

// ---- JSON test shim ----

fn decode_shim<T: for<'de> Deserialize<'de>>(raw: &RawEvent) -> Option<T> {
    serde_json::from_str(&raw.value).ok()
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    hex_bytes(s)?.try_into().ok()
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

#[cfg(test)]
mod tests {
    use stellar_xdr::{Int128Parts, ScBytes, ScMapEntry, ScString, ScSymbol, ScVec, WriteXdr};

    use super::*;

    fn sym(s: &str) -> ScVal {
        ScVal::Symbol(ScSymbol(s.try_into().unwrap()))
    }

    fn sc_bytes(b: &[u8]) -> ScVal {
        ScVal::Bytes(ScBytes(b.to_vec().try_into().unwrap()))
    }

    fn sc_map(entries: Vec<(&str, ScVal)>) -> ScVal {
        let entries: Vec<ScMapEntry> = entries
            .into_iter()
            .map(|(k, val)| ScMapEntry { key: sym(k), val })
            .collect();
        ScVal::Map(Some(ScMap(entries.try_into().unwrap())))
    }

    fn b64(v: &ScVal) -> String {
        v.to_xdr_base64(Limits::none()).unwrap()
    }

    fn raw_event(name: &str, value: ScVal) -> RawEvent {
        RawEvent {
            id: "evt-1".into(),
            contract_id: "CPOOL".into(),
            ledger: 42,
            tx_hash: "abc".into(),
            topic: vec![b64(&sym(name))],
            value: b64(&value),
        }
    }

    #[test]
    fn decodes_xdr_new_commitment() {
        let commitment = [7u8; 32];
        let value = sc_map(vec![
            ("commitment", sc_bytes(&commitment)),
            ("leaf_index", ScVal::U32(5)),
            (
                "deposit_amount",
                ScVal::I128(Int128Parts {
                    hi: 0,
                    lo: 1_000_000,
                }),
            ),
            ("asset", ScVal::String(ScString("USDC".try_into().unwrap()))),
        ]);
        match decode(&raw_event("new_commitment", value)) {
            Some(PoolEvent::NewCommitment {
                commitment: c,
                leaf_index,
                deposit_amount,
                asset,
            }) => {
                assert_eq!(c, commitment);
                assert_eq!(leaf_index, 5);
                assert_eq!(deposit_amount, Some(1_000_000));
                assert_eq!(asset.as_deref(), Some("USDC"));
            }
            other => panic!("wrong decode: {other:?}"),
        }
    }

    #[test]
    fn decodes_xdr_transfer_output_without_amount() {
        let value = sc_map(vec![
            ("commitment", sc_bytes(&[9u8; 32])),
            ("leaf_index", ScVal::U64(6)),
        ]);
        match decode(&raw_event("new_commitment", value)) {
            Some(PoolEvent::NewCommitment {
                deposit_amount,
                asset,
                ..
            }) => {
                assert_eq!(deposit_amount, None);
                assert_eq!(asset, None);
            }
            other => panic!("wrong decode: {other:?}"),
        }
    }

    #[test]
    fn decodes_xdr_nullifier_bare_and_map() {
        let n = [3u8; 32];
        for value in [sc_bytes(&n), sc_map(vec![("nullifier", sc_bytes(&n))])] {
            match decode(&raw_event("nullifier", value)) {
                Some(PoolEvent::NullifierSpent { nullifier }) => assert_eq!(nullifier, n),
                other => panic!("wrong decode: {other:?}"),
            }
        }
    }

    #[test]
    fn decodes_xdr_note() {
        let value = sc_map(vec![
            ("commitment", sc_bytes(&[1u8; 32])),
            ("ephemeral_pubkey", sc_bytes(&[2u8; 33])),
            ("ciphertext", sc_bytes(&[4u8; 120])),
        ]);
        match decode(&raw_event("note", value)) {
            Some(PoolEvent::EncryptedNote {
                commitment,
                ephemeral_pubkey,
                ciphertext,
            }) => {
                assert_eq!(commitment, [1u8; 32]);
                assert_eq!(ephemeral_pubkey.len(), 33);
                assert_eq!(ciphertext.len(), 120);
            }
            other => panic!("wrong decode: {other:?}"),
        }
    }

    #[test]
    fn decodes_xdr_withdrawal_with_negative_capable_i128() {
        let value = sc_map(vec![
            ("amount", ScVal::I128(Int128Parts { hi: 1, lo: 2 })),
            ("asset", sym("XLM")),
        ]);
        match decode(&raw_event("withdrawal", value)) {
            Some(PoolEvent::Withdrawal { amount, asset }) => {
                assert_eq!(amount, (1i128 << 64) | 2);
                assert_eq!(asset, "XLM");
            }
            other => panic!("wrong decode: {other:?}"),
        }
    }

    #[test]
    fn decodes_xdr_issuer() {
        let claim_types = ScVal::Vec(Some(ScVec(
            vec![sym("kyc"), sym("aml")].try_into().unwrap(),
        )));
        let value = sc_map(vec![
            (
                "issuer_id",
                ScVal::String(ScString("iss-1".try_into().unwrap())),
            ),
            ("name", ScVal::String(ScString("Acme".try_into().unwrap()))),
            ("public_key", sc_bytes(&[5u8; 32])),
            ("claim_types", claim_types),
            ("status", sym("active")),
        ]);
        match decode(&raw_event("issuer", value)) {
            Some(PoolEvent::IssuerUpdated {
                issuer_id,
                name,
                public_key,
                claim_types,
                status,
            }) => {
                assert_eq!(issuer_id, "iss-1");
                assert_eq!(name, "Acme");
                assert_eq!(public_key, vec![5u8; 32]);
                assert_eq!(claim_types, vec!["kyc", "aml"]);
                assert_eq!(status, "active");
            }
            other => panic!("wrong decode: {other:?}"),
        }
    }

    #[test]
    fn json_shim_still_decodes_for_tests() {
        let raw = RawEvent {
            id: "evt-2".into(),
            contract_id: "CPOOL".into(),
            ledger: 1,
            tx_hash: "t".into(),
            topic: vec!["nullifier".into()],
            value: format!(r#"{{"nullifier":"0x{}"}}"#, "11".repeat(32)),
        };
        match decode(&raw) {
            Some(PoolEvent::NullifierSpent { nullifier }) => {
                assert_eq!(nullifier, [0x11u8; 32]);
            }
            other => panic!("wrong decode: {other:?}"),
        }
    }

    #[test]
    fn unknown_event_is_skipped() {
        assert!(decode(&raw_event("unrelated_event", ScVal::Void)).is_none());
    }

    #[test]
    fn wrong_shape_is_rejected_not_mangled() {
        // commitment shorter than 32 bytes must not decode
        let value = sc_map(vec![
            ("commitment", sc_bytes(&[7u8; 16])),
            ("leaf_index", ScVal::U32(0)),
        ]);
        assert!(decode(&raw_event("new_commitment", value)).is_none());
    }
}

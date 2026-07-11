//! Shared domain types, database access, and the commitment Merkle tree.
//!
//! Standing invariant (enforced in review, restated here for every reader):
//! nothing in this crate — and nothing that depends on it — may accept,
//! store, or log a plaintext amount, a spending key, or a raw credential.
//! The backend handles public chain state and ciphertext, nothing else.

pub mod config;
pub mod db;
pub mod error;
pub mod merkle;
pub mod models;

pub use error::CoreError;

use std::collections::HashMap;

use attesta_core::{
    config::Config,
    merkle::{MerkleTree, Node, Sha256Hasher},
    models::EncryptedNoteRow,
};
use sqlx::PgPool;
use tokio::sync::{broadcast, Mutex};

pub struct AppState {
    pub db: PgPool,
    pub config: Config,
    /// New encrypted notes are broadcast here for SSE subscribers.
    pub note_tx: broadcast::Sender<EncryptedNoteRow>,
    /// Cached commitment trees, one per pool, topped up incrementally from
    /// the `commitments` table on each tree request. One lock for the whole
    /// map is fine at current scale; requests only hold it for the top-up
    /// query plus O(new leaves · depth) hashing.
    pub trees: Mutex<HashMap<String, PoolTree>>,
}

/// In-memory mirror of one pool's commitment tree.
pub struct PoolTree {
    pub tree: MerkleTree<Sha256Hasher>,
    /// Commitment → leaf index, for O(1) path lookups.
    pub index_by_commitment: HashMap<Node, usize>,
    /// Ledger of the newest leaf — the block anchor clients pin proofs to.
    pub anchored_ledger: i64,
}

impl PoolTree {
    pub fn new() -> Self {
        Self {
            tree: MerkleTree::new(Sha256Hasher),
            index_by_commitment: HashMap::new(),
            anchored_ledger: 0,
        }
    }
}

impl Default for PoolTree {
    fn default() -> Self {
        Self::new()
    }
}

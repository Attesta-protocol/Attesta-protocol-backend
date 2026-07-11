//! Incremental append-only Merkle tree over note commitments.
//!
//! The hash function here is a placeholder (SHA-256) behind the [`TreeHasher`]
//! trait. The production tree must use the same hash as the circuits —
//! a Poseidon instance over the BLS12-381 scalar field, matching the
//! on-chain verifier introduced with Protocol 25. Swapping the hasher is a
//! `security-critical` change requiring dual review (see CONTRIBUTING).

use sha2::{Digest, Sha256};

pub const TREE_DEPTH: usize = 32;

pub type Node = [u8; 32];

pub trait TreeHasher: Send + Sync {
    fn hash_pair(&self, left: &Node, right: &Node) -> Node;
    /// Deterministic value for an empty subtree leaf.
    fn empty_leaf(&self) -> Node;
}

/// Placeholder hasher. NOT the circuit hash — see module docs.
#[derive(Debug, Default, Clone, Copy)]
pub struct Sha256Hasher;

impl TreeHasher for Sha256Hasher {
    fn hash_pair(&self, left: &Node, right: &Node) -> Node {
        let mut h = Sha256::new();
        h.update(left);
        h.update(right);
        h.finalize().into()
    }

    fn empty_leaf(&self) -> Node {
        [0u8; 32]
    }
}

/// A sibling on the path from a leaf to the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathElement {
    pub sibling: Node,
    /// True if the sibling is on the right (i.e. our node is a left child).
    pub sibling_on_right: bool,
}

/// In-memory incremental Merkle tree, rebuilt from the `commitments` table
/// (ordered by `leaf_index`). Fine for testnet scale; a cached-frontier
/// implementation replaces this before mainnet.
pub struct MerkleTree<H: TreeHasher = Sha256Hasher> {
    hasher: H,
    leaves: Vec<Node>,
    /// zeros[d] = root of an empty subtree of depth d.
    zeros: Vec<Node>,
}

impl<H: TreeHasher> MerkleTree<H> {
    pub fn new(hasher: H) -> Self {
        let mut zeros = Vec::with_capacity(TREE_DEPTH + 1);
        zeros.push(hasher.empty_leaf());
        for d in 0..TREE_DEPTH {
            let z = zeros[d];
            zeros.push(hasher.hash_pair(&z, &z));
        }
        Self {
            hasher,
            leaves: Vec::new(),
            zeros,
        }
    }

    pub fn from_leaves(hasher: H, leaves: Vec<Node>) -> Self {
        let mut tree = Self::new(hasher);
        tree.leaves = leaves;
        tree
    }

    pub fn append(&mut self, leaf: Node) -> usize {
        self.leaves.push(leaf);
        self.leaves.len() - 1
    }

    pub fn len(&self) -> usize {
        self.leaves.len()
    }

    pub fn is_empty(&self) -> bool {
        self.leaves.is_empty()
    }

    pub fn root(&self) -> Node {
        let mut level: Vec<Node> = self.leaves.clone();
        for depth in 0..TREE_DEPTH {
            if level.is_empty() {
                return self.zeros[TREE_DEPTH];
            }
            let mut next = Vec::with_capacity(level.len().div_ceil(2));
            for pair in level.chunks(2) {
                let left = pair[0];
                let right = if pair.len() == 2 {
                    pair[1]
                } else {
                    self.zeros[depth]
                };
                next.push(self.hasher.hash_pair(&left, &right));
            }
            level = next;
        }
        level[0]
    }

    /// Merkle path (bottom-up siblings) for the leaf at `index`.
    pub fn path(&self, index: usize) -> Option<Vec<PathElement>> {
        if index >= self.leaves.len() {
            return None;
        }
        let mut path = Vec::with_capacity(TREE_DEPTH);
        let mut level: Vec<Node> = self.leaves.clone();
        let mut idx = index;
        for depth in 0..TREE_DEPTH {
            let sibling_idx = idx ^ 1;
            let sibling = level.get(sibling_idx).copied().unwrap_or(self.zeros[depth]);
            path.push(PathElement {
                sibling,
                sibling_on_right: idx.is_multiple_of(2),
            });

            let mut next = Vec::with_capacity(level.len().div_ceil(2));
            for pair in level.chunks(2) {
                let left = pair[0];
                let right = if pair.len() == 2 {
                    pair[1]
                } else {
                    self.zeros[depth]
                };
                next.push(self.hasher.hash_pair(&left, &right));
            }
            level = next;
            idx /= 2;
        }
        Some(path)
    }

    /// Recompute the root from a leaf and its path — used by tests and by
    /// the disclosure tooling to double-check served paths.
    pub fn verify_path(&self, leaf: Node, path: &[PathElement], root: Node) -> bool {
        let mut node = leaf;
        for el in path {
            node = if el.sibling_on_right {
                self.hasher.hash_pair(&node, &el.sibling)
            } else {
                self.hasher.hash_pair(&el.sibling, &node)
            };
        }
        node == root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(n: u8) -> Node {
        let mut l = [0u8; 32];
        l[0] = n;
        l
    }

    #[test]
    fn empty_tree_has_stable_root() {
        let t = MerkleTree::new(Sha256Hasher);
        assert_eq!(t.root(), t.zeros[TREE_DEPTH]);
    }

    #[test]
    fn paths_verify_against_root() {
        let mut t = MerkleTree::new(Sha256Hasher);
        for n in 1..=5u8 {
            t.append(leaf(n));
        }
        let root = t.root();
        for i in 0..5 {
            let path = t.path(i).expect("path exists");
            assert_eq!(path.len(), TREE_DEPTH);
            assert!(
                t.verify_path(leaf(i as u8 + 1), &path, root),
                "leaf {i} failed"
            );
        }
    }

    #[test]
    fn path_for_missing_leaf_is_none() {
        let t = MerkleTree::new(Sha256Hasher);
        assert!(t.path(0).is_none());
    }

    #[test]
    fn appending_changes_root() {
        let mut t = MerkleTree::new(Sha256Hasher);
        t.append(leaf(1));
        let r1 = t.root();
        t.append(leaf(2));
        assert_ne!(r1, t.root());
    }
}

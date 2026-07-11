//! Incremental append-only Merkle tree over note commitments.
//!
//! The hash function here is a placeholder (SHA-256) behind the [`TreeHasher`]
//! trait. The production tree must use the same hash as the circuits —
//! a Poseidon instance over the BLS12-381 scalar field, matching the
//! on-chain verifier introduced with Protocol 25. Swapping the hasher is a
//! `security-critical` change requiring dual review (see CONTRIBUTING).
//!
//! The tree caches every internal level, so `append` costs O(depth),
//! `root` is O(1), and `path` is O(depth). Memory is ~2n nodes for n
//! leaves. Callers keep one long-lived tree per pool and top it up with
//! newly indexed leaves instead of rebuilding from the database.

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

/// Incremental Merkle tree with cached levels.
///
/// `levels[0]` holds the leaves; `levels[d]` holds the populated nodes at
/// depth `d`. Nodes to the right of the populated region are roots of empty
/// subtrees (`zeros[d]`) and are never materialized.
pub struct MerkleTree<H: TreeHasher = Sha256Hasher> {
    hasher: H,
    levels: Vec<Vec<Node>>,
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
            levels: vec![Vec::new(); TREE_DEPTH + 1],
            zeros,
        }
    }

    pub fn from_leaves(hasher: H, leaves: Vec<Node>) -> Self {
        let mut tree = Self::new(hasher);
        for leaf in leaves {
            tree.append(leaf);
        }
        tree
    }

    /// Append a leaf and update the cached ancestors up to the root.
    pub fn append(&mut self, leaf: Node) -> usize {
        let index = self.levels[0].len();
        self.levels[0].push(leaf);

        let mut idx = index;
        for depth in 0..TREE_DEPTH {
            let parent_idx = idx / 2;
            let left = self.levels[depth][2 * parent_idx];
            let right = self.levels[depth]
                .get(2 * parent_idx + 1)
                .copied()
                .unwrap_or(self.zeros[depth]);
            let parent = self.hasher.hash_pair(&left, &right);

            let parents = &mut self.levels[depth + 1];
            if parent_idx == parents.len() {
                parents.push(parent);
            } else {
                parents[parent_idx] = parent;
            }
            idx = parent_idx;
        }
        index
    }

    pub fn len(&self) -> usize {
        self.levels[0].len()
    }

    pub fn is_empty(&self) -> bool {
        self.levels[0].is_empty()
    }

    pub fn root(&self) -> Node {
        self.levels[TREE_DEPTH]
            .first()
            .copied()
            .unwrap_or(self.zeros[TREE_DEPTH])
    }

    /// Merkle path (bottom-up siblings) for the leaf at `index`.
    pub fn path(&self, index: usize) -> Option<Vec<PathElement>> {
        if index >= self.levels[0].len() {
            return None;
        }
        let mut path = Vec::with_capacity(TREE_DEPTH);
        let mut idx = index;
        for depth in 0..TREE_DEPTH {
            let sibling = self.levels[depth]
                .get(idx ^ 1)
                .copied()
                .unwrap_or(self.zeros[depth]);
            path.push(PathElement {
                sibling,
                sibling_on_right: idx.is_multiple_of(2),
            });
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

    /// Reference root: naive full rebuild, level by level.
    fn naive_root(leaves: &[Node]) -> Node {
        let hasher = Sha256Hasher;
        let mut zeros = vec![hasher.empty_leaf()];
        for d in 0..TREE_DEPTH {
            let z = zeros[d];
            zeros.push(hasher.hash_pair(&z, &z));
        }
        let mut level = leaves.to_vec();
        for depth in 0..TREE_DEPTH {
            if level.is_empty() {
                return zeros[TREE_DEPTH];
            }
            let mut next = Vec::with_capacity(level.len().div_ceil(2));
            for pair in level.chunks(2) {
                let left = pair[0];
                let right = if pair.len() == 2 {
                    pair[1]
                } else {
                    zeros[depth]
                };
                next.push(hasher.hash_pair(&left, &right));
            }
            level = next;
        }
        level[0]
    }

    #[test]
    fn empty_tree_has_stable_root() {
        let t = MerkleTree::new(Sha256Hasher);
        assert_eq!(t.root(), t.zeros[TREE_DEPTH]);
    }

    #[test]
    fn incremental_root_matches_naive_rebuild() {
        let mut t = MerkleTree::new(Sha256Hasher);
        let mut leaves = Vec::new();
        for n in 1..=9u8 {
            leaves.push(leaf(n));
            t.append(leaf(n));
            assert_eq!(t.root(), naive_root(&leaves), "diverged at {n} leaves");
        }
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
    fn old_paths_still_verify_after_later_appends() {
        let mut t = MerkleTree::new(Sha256Hasher);
        for n in 1..=3u8 {
            t.append(leaf(n));
        }
        t.append(leaf(4));
        let root = t.root();
        // Paths must be re-fetched against the new root and still verify.
        for i in 0..4 {
            let path = t.path(i).expect("path exists");
            assert!(t.verify_path(leaf(i as u8 + 1), &path, root));
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

    #[test]
    fn from_leaves_matches_appends() {
        let leaves: Vec<Node> = (1..=6u8).map(leaf).collect();
        let t1 = MerkleTree::from_leaves(Sha256Hasher, leaves.clone());
        let mut t2 = MerkleTree::new(Sha256Hasher);
        for l in leaves {
            t2.append(l);
        }
        assert_eq!(t1.root(), t2.root());
        assert_eq!(t1.len(), t2.len());
    }
}

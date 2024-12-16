use crate::bmt::{HASH_SIZE, ZERO_SEGMENT};
use alloy_primitives::keccak256;
use std::cell::UnsafeCell;
use std::sync::{
    atomic::{AtomicI32, Ordering},
    Arc,
};

use super::DEFAULT_MAX_PAYLOAD_SIZE;

/// A reusable control structure representing a BMT organised in a binary tree
#[derive(Debug)]
pub struct Tree {
    /// Leaf nodes of the tree, other nodes accessible via parent links
    pub(crate) leaves: Vec<Arc<Node>>,
    pub(crate) buffer: UnsafeCell<[u8; DEFAULT_MAX_PAYLOAD_SIZE]>,
}

impl Tree {
    /// Initialises a tree by building up the nodes of a BMT
    pub(crate) fn new() -> Self {
        let root = Arc::new(Node::new(0, None));

        let mut prev_level = vec![root];

        // Iterate over levels and creates 2^(depth-level) nodes
        let mut count = 2;
        for _level in (0..=crate::bmt::pool::DEPTH - 2).rev() {
            let mut nodes = Vec::with_capacity(count);

            for i in 0..count {
                let parent = Some(&prev_level[i / 2]);
                let node = Arc::new(Node::new(i, parent.cloned()));
                nodes.push(node);
            }
            prev_level = nodes;
            count <<= 1;
        }

        // The datanode level is the nodes on the last level
        Self {
            leaves: prev_level,
            buffer: UnsafeCell::new([0u8; DEFAULT_MAX_PAYLOAD_SIZE]),
        }
    }
}

/// A reusable segment hasher representing a node in a BMT.
#[derive(Debug)]
pub struct Node {
    /// Whether it is left side of the parent double segment
    pub(crate) is_left: bool,
    /// Pointer to parent node in the BMT
    pub(crate) parent: Option<Arc<Node>>,
    /// Left child segment
    left: [u8; HASH_SIZE],
    /// Right child segment
    right: [u8; HASH_SIZE],
    /// Atomic state toggle for concurrency control
    state: AtomicI32,
}

unsafe impl Send for Node {}
unsafe impl Sync for Node {}

impl Node {
    /// Constructs a segment hasher node in the BMT
    pub(crate) fn new(index: usize, parent: Option<Arc<Node>>) -> Self {
        Self {
            parent,
            is_left: index % 2 == 0,
            left: ZERO_SEGMENT,
            right: ZERO_SEGMENT,
            state: AtomicI32::new(0),
        }
    }

    /// Sets the parent of the current node
    pub(crate) fn set_parent(&mut self, parent: Arc<Node>) {
        self.parent = Some(parent);
    }

    /// Updates the respective child segment
    pub(crate) fn set(&mut self, is_left: bool, segment: [u8; HASH_SIZE]) {
        match is_left {
            true => self.left = segment,
            false => self.right = segment,
        }
    }

    /// Returns the respective child segment
    pub(crate) fn segment(&self, is_left: bool) -> [u8; HASH_SIZE] {
        match is_left {
            true => self.left,
            false => self.right,
        }
    }

    /// Atomically toggles the node's state and returns true if it's now active
    pub(crate) fn toggle(&self) -> bool {
        self.state.fetch_add(1, Ordering::SeqCst) % 2 == 1
    }

    /// A utility hashing function that returns the hash of a segment pair in a node
    pub(crate) fn hash_segment(&self) -> [u8; 32] {
        let mut buffer = [0u8; HASH_SIZE * 2];
        buffer[..HASH_SIZE].copy_from_slice(&self.left);
        buffer[HASH_SIZE..].copy_from_slice(&self.right);

        *keccak256(buffer)
    }
}

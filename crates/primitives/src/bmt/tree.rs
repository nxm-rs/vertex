use crate::{
    bmt::{DEPTH, HASH_SIZE, SEGMENT_PAIR_SIZE},
    BMT_BRANCHES, SEGMENT_SIZE,
};
use alloy_primitives::keccak256;
use std::sync::{atomic::AtomicBool, Arc};
use tokio::sync::Mutex;

use super::Segment;

/// A reusable control structure representing a BMT organised in a binary tree
#[derive(Debug)]
pub struct Tree {
    /// Leaf nodes of the tree, other nodes accessible via parent links
    pub(crate) leaves: Vec<Arc<Mutex<Node>>>,
    pub(crate) buffer: [u8; BMT_BRANCHES * SEGMENT_SIZE],
}

impl Tree {
    /// Initialises a tree by building up the nodes of a BMT
    pub(crate) fn new() -> Self {
        let root = Arc::new(Mutex::new(Node::new(None)));

        let mut prev_level = vec![root];

        // Iterate over levels and creates 2^(depth-level) nodes
        let mut count = 2;
        let mut level = (DEPTH as isize) - 2;
        while level >= 0 {
            let mut nodes = Vec::with_capacity(count);

            // use weird iteration loop to avoid bounds checks when determining the 'parent' node.
            prev_level.iter().for_each(|parent| {
                // create 2 nodes (left and right)
                for _ in 0..2 {
                    let node = Arc::new(Mutex::new(Node::new(Some(parent.clone()))));
                    nodes.push(node);
                }
            });

            prev_level = nodes;
            count <<= 1;
            level -= 1;
        }

        // The datanode level is the nodes on the last level
        Self {
            leaves: prev_level,
            buffer: [0u8; BMT_BRANCHES * SEGMENT_SIZE],
        }
    }
}

/// A reusable segment hasher representing a node in a BMT.
#[derive(Debug, Default)]
pub struct Node {
    /// Pointer to parent node in the BMT
    parent: Option<Arc<Mutex<Node>>>,
    /// Left child segment
    left: Option<Segment>,
    /// Right child segment
    right: Option<Segment>,
    // Atomic state
    state: AtomicBool,
}

impl Node {
    /// Constructs a segment hasher node in the BMT
    pub(crate) fn new(parent: Option<Arc<Mutex<Node>>>) -> Self {
        Self {
            parent,
            state: AtomicBool::new(true),
            ..Default::default()
        }
    }

    /// Gets the parent of the current node
    pub(crate) fn parent(&self) -> Option<Arc<Mutex<Node>>> {
        self.parent.clone()
    }

    /// Updates the respective child segment
    pub(crate) fn set(&mut self, is_left: bool, segment: Segment) {
        match is_left {
            true => self.left = Some(segment),
            false => self.right = Some(segment),
        }
    }

    pub(crate) fn toggle(&mut self) -> bool {
        self.state.fetch_not(std::sync::atomic::Ordering::SeqCst)
    }

    /// Returns the respective child segment
    pub(crate) fn segment(&self, is_left: bool) -> Option<Segment> {
        match is_left {
            true => self.left,
            false => self.right,
        }
    }

    /// A utility hashing function that returns the hash of a segment pair in a node
    pub(crate) fn hash_segment(&self) -> Segment {
        let mut buffer = [0u8; SEGMENT_PAIR_SIZE];
        buffer[..HASH_SIZE].copy_from_slice(&self.left.unwrap());
        buffer[HASH_SIZE..].copy_from_slice(&self.right.unwrap());

        *keccak256(buffer)
    }
}

pub(crate) fn is_left(level: usize, index: usize) -> bool {
    let effective_index = index / (1 << (level - 1));
    effective_index % 2 == 0
}

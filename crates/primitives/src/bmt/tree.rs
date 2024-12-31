use crate::{bmt::HASH_SIZE, SEGMENT_SIZE};
use alloy_primitives::keccak256;
use std::sync::Arc;
use tokio::sync::Mutex;

use super::{Segment, ZERO_SEGMENT_PAIR};

/// A reusable control structure representing a BMT organised in a binary tree
#[derive(Debug)]
pub struct Tree<const W: usize, const DEPTH: usize>
where
    [(); W * SEGMENT_SIZE]:,
    [(); DEPTH + 1]:,
{
    /// Leaf nodes of the tree, other nodes accessible via parent links
    pub(crate) leaves: Vec<Arc<Mutex<Node>>>,
    pub(crate) buffer: [u8; W * SEGMENT_SIZE],
}

impl<const W: usize, const DEPTH: usize> Tree<W, DEPTH>
where
    [(); W * SEGMENT_SIZE]:,
    [(); DEPTH + 1]:,
{
    /// Initialises a tree by building up the nodes of a BMT
    pub(crate) fn new() -> Self {
        let root = Arc::new(Mutex::new(Node::new(0, None)));

        let mut prev_level = vec![root];

        // Iterate over levels and creates 2^(depth-level) nodes
        let mut count = 2;
        let mut level = (DEPTH as isize) - 2;
        while level >= 0 {
            let mut nodes = Vec::with_capacity(count);

            for i in 0..count {
                let parent = Some(&prev_level[i / 2]);
                let node = Arc::new(Mutex::new(Node::new(i, parent.cloned())));
                nodes.push(node);
            }
            prev_level = nodes;
            count <<= 1;
            level -= 1;
        }

        // The datanode level is the nodes on the last level
        Self {
            leaves: prev_level,
            buffer: [0u8; W * SEGMENT_SIZE],
        }
    }
}

/// A reusable segment hasher representing a node in a BMT.
#[derive(Debug, Default)]
pub struct Node {
    /// Whether it is left side of the parent double segment
    pub(crate) is_left: bool,
    /// Pointer to parent node in the BMT
    parent: Option<Arc<Mutex<Node>>>,
    /// Left child segment
    left: Option<Segment>,
    /// Right child segment
    right: Option<Segment>,
}

impl Node {
    /// Constructs a segment hasher node in the BMT
    pub(crate) fn new(index: usize, parent: Option<Arc<Mutex<Node>>>) -> Self {
        Self {
            parent,
            is_left: index % 2 == 0,
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

    /// Returns the respective child segment
    pub(crate) fn segment(&self, is_left: bool) -> Option<Segment> {
        match is_left {
            true => self.left,
            false => self.right,
        }
    }

    /// A utility hashing function that returns the hash of a segment pair in a node
    pub(crate) fn hash_segment(&self) -> Segment {
        let mut buffer = ZERO_SEGMENT_PAIR;
        buffer[..HASH_SIZE].copy_from_slice(&self.left.unwrap());
        buffer[HASH_SIZE..].copy_from_slice(&self.right.unwrap());

        *keccak256(buffer)
    }
}

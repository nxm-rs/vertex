use crate::{bmt::SEGMENT_PAIR_SIZE, BMT_BRANCHES, SEGMENT_SIZE};
use std::sync::atomic::AtomicBool;

use super::Segment;

/// A reusable control structure representing a BMT organised in a binary tree
#[derive(Debug)]
pub struct Tree<const N: usize>
where
    [(); capacity(N)]:,
{
    /// AtomicBool for self-managed concurrency between threads
    pub(crate) state: [AtomicBool; capacity(N)],
    /// All nodes within the BMT, excluding level 0
    pub(crate) leaves: [Segment; capacity(N)],
    /// Nodes within the BMT on level 0 that correspond to byte sequences that are written by the
    /// Write trait.
    pub(crate) buffer: [u8; BMT_BRANCHES * SEGMENT_SIZE],
}

impl<const N: usize> Tree<N>
where
    [(); capacity(N)]:,
{
    /// Initialises a tree by building up the nodes of a BMT
    pub(crate) fn new() -> Self {
        Self {
            state: [const { AtomicBool::new(false) }; capacity(N)],
            leaves: [[0u8; SEGMENT_SIZE]; capacity(N)],
            buffer: [0u8; BMT_BRANCHES * SEGMENT_SIZE],
        }
    }
}

pub struct TreeIterator<'a, const N: usize>
where
    [(); capacity(N)]:,
{
    tree: &'a mut Tree<N>,
    current_level: usize,
    current_index: usize,
}

impl<'a, const N: usize> TreeIterator<'a, N>
where
    [(); capacity(N)]:,
{
    pub fn new(tree: &'a mut Tree<N>, segment_pair_index: usize) -> Self {
        let current_level = 1; // Start at level 1
        let current_index = segment_pair_index;
        Self {
            tree,
            current_level,
            current_index,
        }
    }
}

impl<'a, const N: usize> Iterator for TreeIterator<'a, N>
where
    [(); capacity(N)]:,
{
    /// The iterator returns:
    /// - Non-mutable references to the left and right children.
    /// - A mutable reference to the atomic state of the current node.
    /// - A mutable reference to the parent node.
    /// - Whether the node at this level is on the left or right (bool)
    type Item = (
        (&'a [u8], &'a [u8]),
        &'a mut AtomicBool,
        &'a mut Segment,
        bool,
    );

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_index == 0 && self.current_level > 1 {
            // Termination: Reached the root
            return None;
        }

        // Calculate the flat index of the current node
        let flat_index = self.current_index + ((1 << (self.current_level - 1)) - 1);

        // Determine if the node is left or right
        let is_left = self.current_index % 2 == 0;

        if self.current_level == 1 {
            // Special case: Level 1 (childen come from the buffer)
            let start = self.current_index * SEGMENT_PAIR_SIZE;
            let left_child = &self.tree.buffer[start..start + SEGMENT_SIZE];
            let right_child = &self.tree.buffer[start + SEGMENT_SIZE..start + SEGMENT_PAIR_SIZE];
            let current_state = unsafe { self.tree.state.get_unchecked_mut(flat_index) };
            let parent_node = unsafe { self.tree.leaves.get_unchecked_mut(self.current_index / 2) };

            // Update index and level
            self.current_index /= 2;
            self.current_level += 1;

            return Some((
                (left_child, right_child),
                current_state,
                parent_node,
                is_left,
            ));
        }

        // General case: Levels 2 and above (childen come from leaves)
        let left_child_index = self.current_index * 2;
        let right_child_index = left_child_index + 1;

        let left_child = unsafe { self.tree.leaves.get_unchecked(left_child_index) };
        let right_child = unsafe { self.tree.leaves.get_unchecked(right_child_index) };

        let current_state = unsafe { self.tree.state.get_unchecked_mut(flat_index) };
        let parent_index = self.current_index / 2;
        let parent_node = unsafe {
            self.tree
                .leaves
                .get_unchecked_mut(((1 << (self.current_level - 2)) - 1) + parent_index)
        };

        // Update index and level
        self.current_index /= 2;
        self.current_level += 1;

        Some((
            (left_child, right_child),
            current_state,
            parent_node,
            is_left,
        ))
    }
}

/// When traversing the BMT from level 1, given a nominated segment index, and a corresponding
/// level, determine if the node on the path is left or right in the BMT.
#[inline]
pub(crate) fn is_left(level: usize, index: usize) -> bool {
    (index / (1 << (level - 1))) % 2 == 0
}

/// Given the number of segments on the bottom layer of a BMT tree, return the total capacity of
/// the BMT tree, excluding the bottom level.
pub(crate) const fn capacity(num: usize) -> usize {
    // Total capacity = 2^num - 1
    (2 ^ num) - num - 1
}

///// A reusable segment hasher representing a node in a BMT.
//#[derive(Debug, Default)]
//pub struct Node {
//    /// Left child segment
//    left: Option<Segment>,
//    /// Right child segment
//    right: Option<Segment>,
//}
//
//impl Node {
//    /// Constructs a segment hasher node in the BMT
//    pub(crate) fn new() -> Self {
//        Self {
//            ..Default::default()
//        }
//    }
//
//    /// Gets the parent of the current node
//    pub(crate) fn parent(&self) -> Option<Segment> {
//        todo!("return the parent of the node");
//    }
//
//    /// Updates the respective child segment
//    pub(crate) fn set(&mut self, is_left: bool, segment: Segment) {
//        match is_left {
//            true => self.left = Some(segment),
//            false => self.right = Some(segment),
//        }
//    }
//
//    pub(crate) fn toggle(&mut self) -> bool {
//        self.state.fetch_not(std::sync::atomic::Ordering::SeqCst)
//    }
//
//    /// Returns the respective child segment
//    pub(crate) fn segment(&self, is_left: bool) -> Option<Segment> {
//        match is_left {
//            true => self.left,
//            false => self.right,
//        }
//    }
//
//    /// A utility hashing function that returns the hash of a segment pair in a node
//    pub(crate) fn hash_segment(&self) -> Segment {
//        let mut buffer = [0u8; SEGMENT_PAIR_SIZE];
//        buffer[..HASH_SIZE].copy_from_slice(&self.left.unwrap());
//        buffer[HASH_SIZE..].copy_from_slice(&self.right.unwrap());
//
//        *keccak256(&buffer)
//    }
//}

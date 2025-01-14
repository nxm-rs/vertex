use crate::{BMT_BRANCHES, SEGMENT_SIZE};
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, Ordering};

use super::Segment;

/// Calculates the depth of a Binary Merkle Tree given the number of nodes on the bottom level
/// (0-indexed), rounded up to the nearest power of two.
const fn calculate_depth(num_bottom_level: usize) -> usize {
    let mut depth = 0;
    let mut segments = num_bottom_level;
    while segments > 1 {
        segments /= 2;
        depth += 1;
    }
    depth + 1 // Include level 0
}

/// Generates index offset lookup tables given some Binary Merkle Tree's depth.
const fn generate_offset_table<const DEPTH: usize>() -> [usize; DEPTH] {
    let mut offsets = [0; DEPTH];
    let mut current_offset = 0;
    let mut level = 0;

    while level < DEPTH {
        offsets[level] = current_offset;
        current_offset += 1 << (DEPTH - level - 1);
        level += 1;
    }

    offsets
}

const DEPTH: usize = calculate_depth(BMT_BRANCHES);

/// Determine leaf constants at compile time
/// The leaves element for the data structure dismisses the Level 0 part of the BMT (which is
/// handled by the `buf`). Therefore, the leaves component is essentially concerned about a tree
/// with a depth of DEPTH - 1.
const LEAVES_DEPTH: usize = DEPTH - 1;
const LEAVES_CAPACITY: usize = (1 << LEAVES_DEPTH) - 1;
const LEAVES_LEVEL_OFFSETS: [usize; LEAVES_DEPTH] = generate_offset_table::<LEAVES_DEPTH>();

// Exclude levels 0 and
/// The states element for the data structure dismisses the Level 0 (they are not the parent of any
/// node), and the Level 1 (these are by default able to be processed as their children are known).
/// Therefore the state component is essentially concerned about a tree with a depth of DEPTH - 2.
const STATE_DEPTH: usize = DEPTH - 2;
const STATE_CAPACITY: usize = (1 << (STATE_DEPTH)) - 1;
const STATE_LEVEL_OFFSETS: [usize; STATE_DEPTH] = generate_offset_table::<STATE_DEPTH>();

/// A reusable control structure representing a BMT organised in a binary tree
#[derive(Debug)]
pub struct Tree {
    /// AtomicBool for self-managed concurrency between threads, excluding level 0 and level 1
    state: [UnsafeCell<AtomicBool>; STATE_CAPACITY],
    /// All nodes within the BMT, excluding level 0
    leaves: [UnsafeCell<Segment>; LEAVES_CAPACITY],
    /// Nodes within the BMT on level 0 that correspond to byte sequences that are written by the
    /// Write trait.
    pub(crate) buf: [u8; BMT_BRANCHES * SEGMENT_SIZE],
}

impl Default for Tree {
    fn default() -> Self {
        Self {
            state: [const { UnsafeCell::new(AtomicBool::new(true)) }; STATE_CAPACITY],
            leaves: [const { UnsafeCell::new([0u8; SEGMENT_SIZE]) }; LEAVES_CAPACITY],
            buf: [0u8; BMT_BRANCHES * SEGMENT_SIZE],
        }
    }
}

unsafe impl Sync for Tree {}

impl Tree {
    /// Create a new [`Tree`] by populating the arrays with their defaults.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Get mutable access to a leaf node (unsafe due to raw pointer usage)
    pub(crate) fn get_leaf_mut(&self, index: usize) -> *mut Segment {
        self.leaves[index].get()
    }

    /// Get mutable access to a state node (unsafe due to raw pointer usage)
    pub(crate) fn get_state_mut(&self, index: usize) -> *mut AtomicBool {
        self.state[index].get()
    }

    /// Reset the structure of the tree so that it can be re-used.
    pub(crate) fn reset(&self) {
        for cell in self.state.iter() {
            unsafe {
                (*cell.get()).store(true, Ordering::SeqCst);
            }
        }

        for cell in self.leaves.iter() {
            unsafe {
                let leaf = &mut *cell.get();
                leaf.fill(0);
            }
        }
    }
}

/// A [`TreeIterator`] that when given a node's index on **Level 1** of the Binary Merkle Tree,
/// will subseqeuntly traverse the tree through to the root.
pub struct TreeIterator<'a> {
    /// The Binary Merkle Tree for traversal,
    tree: &'a Tree,
    /// The current level (0-indexed) in the Binary Merkle Tree for the iterator.
    level: usize,
    /// The current index (0-indexed) of the node on `level` in the Binary Merkle Tree.
    index: usize,
}

impl<'a> TreeIterator<'a> {
    pub fn new(tree: &'a Tree, i: usize) -> Self {
        assert!(
            i < BMT_BRANCHES,
            "Invalid index, must be less than BMT_BRANCHES"
        );
        Self {
            tree,
            level: 1, // Start at level 1
            index: i,
        }
    }
}

impl<'a> Iterator for TreeIterator<'a> {
    /// The iterator returns:
    /// - A mutable reference to the CURRENT NODE
    /// - References to the left and right CHILDREN
    /// - Atomic state of the PARENT node (None at the root)
    /// - The current level that is being processed
    /// - The current index that is being processed
    type Item = (
        *mut Segment,
        (&'a [u8], &'a [u8]),
        Option<*mut AtomicBool>,
        usize,
        usize,
    );

    fn next(&mut self) -> Option<Self::Item> {
        if self.level >= DEPTH {
            return None;
        }

        // Determine the left and right children
        let (left, right) = if self.level == 1 {
            // Level 1: Children come from the buffer
            let offset = self.index * (SEGMENT_SIZE * 2);
            unsafe {
                (
                    &*(self.tree.buf[offset..offset + SEGMENT_SIZE].as_ptr()
                        as *const [u8; SEGMENT_SIZE]),
                    &*(self.tree.buf[offset + SEGMENT_SIZE..offset + (SEGMENT_SIZE * 2)].as_ptr()
                        as *const [u8; SEGMENT_SIZE]),
                )
            }
        } else {
            // Level >= 2: Children come from the previous level's leaves
            let offset = LEAVES_LEVEL_OFFSETS[self.level - 2];
            unsafe {
                (
                    &*self.tree.get_leaf_mut(offset + self.index * 2),
                    &*self.tree.get_leaf_mut(offset + self.index * 2 + 1),
                )
            }
        };

        // Determine the current node
        let node = self
            .tree
            .get_leaf_mut(LEAVES_LEVEL_OFFSETS[self.level - 1] + self.index);

        // Determine the parent state
        let parent_state = if self.level == DEPTH - 1 {
            // Root level has no parent
            None
        } else {
            let parent_index = self.index / 2;
            Some(
                self.tree
                    .get_state_mut(STATE_LEVEL_OFFSETS[self.level - 1] + parent_index),
            )
        };

        let current_level = self.level;
        let current_index = self.index;

        self.index /= 2;
        self.level += 1;

        Some((
            node,
            (left, right),
            parent_state,
            current_level,
            current_index,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_macro_initialization() {
        const EXPECTED_DEPTH: usize = 8; // Levels 0 to 7 inclusive
        const EXPECTED_OFFSETS: [usize; EXPECTED_DEPTH - 1] = [0, 64, 96, 112, 120, 124, 126];

        assert_eq!(
            DEPTH, EXPECTED_DEPTH,
            "DEPTH should be 8 for 128 nodes at level 0"
        );

        for (i, &expected_offset) in EXPECTED_OFFSETS.iter().enumerate() {
            assert_eq!(
                LEAVES_LEVEL_OFFSETS[i], expected_offset,
                "OFFSET_LOOKUP[{}] should match expected value",
                i
            );
        }
    }
}

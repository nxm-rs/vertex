use crate::{bmt::SEGMENT_PAIR_SIZE, BMT_BRANCHES, SEGMENT_SIZE};
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, Ordering};

use super::Segment;

/// Macro to generate constants dynamically:
/// 1. `LEAVES_CAPACITY`
/// 2. `STATE_CAPACITY`
/// 3. `OFFSET_LOOKUP`
macro_rules! generate_tree_capacities {
    ($bottom_level_segments:expr, $segment_size:expr, $hash_fn:expr) => {
        pub(crate) const DEPTH: usize = {
            let mut depth = 0;
            let mut segments = $bottom_level_segments;
            while segments > 1 {
                segments /= 2;
                depth += 1;
            }
            depth + 1 // Include level 0
        };

        // Type-level assertion to enforce DEPTH > 2
        #[allow(dead_code)]
        struct DepthMustBeGreaterThanTwo;
        impl DepthMustBeGreaterThanTwo {
            const ASSERT: () = if DEPTH > 2 {
            } else {
                panic!(
                    "DEPTH of 2 is invalid: the tree must have more than one level above the root."
                );
            };
        }

        // Exclude level 0
        const LEAVES_CAPACITY: usize = (1 << (DEPTH - 1)) - 1;
        // Exclude levels 0 and 1
        const STATE_CAPACITY: usize = (1 << (DEPTH - 2)) - 1;
    };
}

macro_rules! generate_offset_table {
    ($depth:expr) => {{
        const fn offsets() -> [usize; $depth] {
            let mut offsets = [0; $depth];
            let mut current_offset = 0;
            let mut level = 0;

            while level < $depth {
                offsets[level] = current_offset;
                current_offset += 1 << ($depth - level - 1);
                level += 1;
            }

            offsets
        }

        offsets()
    }};
}

const LEAF_OFFSETS: [usize; DEPTH - 1] = generate_offset_table!(DEPTH - 1);
const STATE_OFFSETS: [usize; DEPTH - 2] = generate_offset_table!(DEPTH - 2);

generate_tree_capacities!(128, 32, keccak256);

/// A reusable control structure representing a BMT organised in a binary tree
#[derive(Debug)]
pub struct Tree {
    /// AtomicBool for self-managed concurrency between threads
    state: [UnsafeCell<AtomicBool>; STATE_CAPACITY],
    /// All nodes within the BMT, excluding level 0
    leaves: [UnsafeCell<Segment>; LEAVES_CAPACITY],
    /// Nodes within the BMT on level 0 that correspond to byte sequences that are written by the
    /// Write trait.
    pub(crate) buffer: [u8; BMT_BRANCHES * SEGMENT_SIZE],
}

unsafe impl Sync for Tree {}

impl Tree {
    /// Initialises a tree by building up the nodes of a BMT
    pub(crate) fn new() -> Self {
        Self {
            state: [const { UnsafeCell::new(AtomicBool::new(true)) }; STATE_CAPACITY],
            leaves: [const { UnsafeCell::new([0u8; SEGMENT_SIZE]) }; LEAVES_CAPACITY],
            buffer: [0u8; BMT_BRANCHES * SEGMENT_SIZE],
        }
    }

    /// Get mutable access to a leaf node (unsafe due to raw pointer usage)
    pub(crate) fn get_leaf_mut(&self, index: usize) -> *mut Segment {
        self.leaves[index].get()
    }

    /// Get mutable access to a state node (unsafe due to raw pointer usage)
    pub(crate) fn get_state_mut(&self, index: usize) -> *mut AtomicBool {
        self.state[index].get()
    }

    pub(crate) fn state_reset(&self) {
        for state_cell in self.state.iter() {
            unsafe {
                (*state_cell.get()).store(true, Ordering::SeqCst);
            }
        }

        for leaf_cell in self.leaves.iter() {
            unsafe {
                let leaf = &mut *leaf_cell.get();
                leaf.fill(0);
            }
        }
    }
}

pub struct TreeIterator<'a> {
    tree: &'a Tree,
    pub(crate) current_level: usize,
    current_index: usize,
}

impl<'a> TreeIterator<'a> {
    pub fn new(tree: &'a Tree, segment_pair_index: usize) -> Self {
        let current_level = 1; // Start at level 1
        let current_index = segment_pair_index;
        Self {
            tree,
            current_level,
            current_index,
        }
    }
}

impl<'a> Iterator for TreeIterator<'a> {
    /// The iterator returns:
    /// - A mutable reference to the CURRENT NODE
    /// - References to the left and right CHILDREN
    /// - Atomic state of the PARENT node (None at the root)
    type Item = (*mut Segment, (&'a [u8], &'a [u8]), Option<*mut AtomicBool>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_level >= DEPTH {
            return None;
        }

        // Determine the left and right children
        let (left_child, right_child) = if self.current_level == 1 {
            // Level 1: Children come from the buffer
            let start = self.current_index * SEGMENT_PAIR_SIZE;
            unsafe {
                (
                    &*(self.tree.buffer[start..start + SEGMENT_SIZE].as_ptr()
                        as *const [u8; SEGMENT_SIZE]),
                    &*(self.tree.buffer[start + SEGMENT_SIZE..start + SEGMENT_PAIR_SIZE].as_ptr()
                        as *const [u8; SEGMENT_SIZE]),
                )
            }
        } else {
            // Level >= 2: Children come from the previous level's leaves
            let child_base = LEAF_OFFSETS[self.current_level - 2];
            unsafe {
                (
                    &*self.tree.get_leaf_mut(child_base + self.current_index * 2),
                    &*self
                        .tree
                        .get_leaf_mut(child_base + self.current_index * 2 + 1),
                )
            }
        };

        // Determine the current node
        let current_node = self
            .tree
            .get_leaf_mut(self.current_index + LEAF_OFFSETS[self.current_level - 1]);

        // Determine the parent state
        let parent_state = if self.current_level == DEPTH - 1 {
            // Root level has no parent
            None
        } else {
            let parent_index = self.current_index / 2;
            Some(
                self.tree
                    .get_state_mut(parent_index + STATE_OFFSETS[self.current_level - 1]),
            )
        };

        self.current_index /= 2;
        self.current_level += 1;

        Some((current_node, (left_child, right_child), parent_state))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_macro_initialization() {
        const EXPECTED_DEPTH: usize = 8; // Levels 0 to 7 inclusive
        const EXPECTED_OFFSETS: [usize; EXPECTED_DEPTH - 1] = [0, 64, 96, 112, 120, 124, 126];

        println!("Leaf offsets: {:?}", LEAF_OFFSETS);
        println!("State offsets: {:?}", STATE_OFFSETS);
        println!("Leaves capacity: {}", LEAVES_CAPACITY);
        println!("State capacity: {}", STATE_CAPACITY);

        assert_eq!(
            DEPTH, EXPECTED_DEPTH,
            "DEPTH should be 8 for 128 nodes at level 0"
        );

        for (i, &expected_offset) in EXPECTED_OFFSETS.iter().enumerate() {
            assert_eq!(
                LEAF_OFFSETS[i], expected_offset,
                "OFFSET_LOOKUP[{}] should match expected value",
                i
            );
        }
    }
}

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use swarm_primitives_traits::{Segment, BRANCHES, SEGMENT_SIZE};

/// Calculate the depth of a BMT given the number of nodes `n` on the bottom level (0-indexed),
/// rounded up to the nearest power of two.
const fn calculate_depth(n: usize) -> usize {
    let mut depth = 0;
    let mut segments = n;
    while segments > 1 {
        segments /= 2;
        depth += 1;
    }
    depth + 1 // Include level 0
}

/// Generates index offset lookup tables given some BMT's depth.
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

/// Set the depth of the BMT tree, based on configuration variables.
pub const DEPTH: usize = calculate_depth(BRANCHES);

/// Determine leaf constants at compile time
/// The leaves element for the data structure dismisses the Level 0 part of the BMT (which is
/// handled by `buf`). Therefore, the leaves component is essentially concerned about a BMT
/// with a depth of DEPTH - 1.
const LEAVES_DEPTH: usize = DEPTH - 1;
const LEAVES_CAPACITY: usize = (1 << LEAVES_DEPTH) - 1;
const LEAVES_LEVEL_OFFSETS: [usize; LEAVES_DEPTH] = generate_offset_table::<LEAVES_DEPTH>();

// Exclude levels 0 and and 1
/// The state element for the [`Tree`] struct includes atomic state variables for each node at
/// DEPTH >= 2. Reasoning for omitting levels:
/// * Level 0: Omitted as these nodes are not the parent of any other node.
/// * Level 1: Omitted as these nodes are the parents of a pair of level 0 nodes, however there is
/// no need to evaluate the state whether or not it has been hashed as this represents the initial
/// level from which the algorithm starts at.
const STATE_DEPTH: usize = DEPTH - 2;
const STATE_CAPACITY: usize = (1 << (STATE_DEPTH)) - 1;
const STATE_LEVEL_OFFSETS: [usize; STATE_DEPTH] = generate_offset_table::<STATE_DEPTH>();

/// A reusable control structure representing a BMT organised in a flat array.
#[derive(Debug)]
pub struct Tree {
    /// AtomicBool for self-managed concurrency between threads, excluding levels 0 and 1.
    state: [UnsafeCell<AtomicBool>; STATE_CAPACITY],
    /// All nodes within the BMT, excluding level 0.
    leaves: [UnsafeCell<Segment>; LEAVES_CAPACITY],
    /// Nodes within the BMT on level 0 correspond to byte sequences.
    pub(crate) buf: UnsafeCell<[u8; BRANCHES * SEGMENT_SIZE]>,
}

impl Default for Tree {
    fn default() -> Self {
        Self {
            state: [const { UnsafeCell::new(AtomicBool::new(true)) }; STATE_CAPACITY],
            leaves: [const { UnsafeCell::new([0u8; SEGMENT_SIZE]) }; LEAVES_CAPACITY],
            buf: const { UnsafeCell::new([0u8; BRANCHES * SEGMENT_SIZE]) },
        }
    }
}

// SAFETY: Sync is implemented because all internal mutable states are access via atomic
// operations or through UnsafeCell, ensuring no aliasing.
unsafe impl Sync for Tree {}

impl Tree {
    /// Create a new [`Tree`] by populating the arrays with their defaults.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Safely get an immutable reference to a buffer segment as a `Segment`
    pub(crate) fn get_buf_segment(&self, offset: usize) -> &Segment {
        unsafe {
            // SAFETY: The caller ensures the offset and size are valid.
            &*(&(*self.buf.get())[offset..offset + SEGMENT_SIZE] as *const [u8] as *const Segment)
        }
    }

    /// Safely get a mutable reference to a segment in the buffer
    pub(crate) fn get_buf_segment_mut(&self, offset: usize) -> &mut Segment {
        unsafe {
            // SAFETY: The caller ensures the offset and size are valid, and that no other mutable
            // or immutable references exist to this region.
            &mut *(&mut (*self.buf.get())[offset..offset + SEGMENT_SIZE] as *mut [u8]
                as *mut Segment)
        }
    }

    /// Safely get an immutable reference to a leaf segment.
    pub(crate) fn get_leaf(&self, index: usize) -> &Segment {
        unsafe {
            // SAFETY: The caller ensures that `index` is valid and within bounds.
            &*self.leaves[index].get()
        }
    }

    /// Safely get a mutable reference to a leaf segment.
    pub(crate) fn get_leaf_mut(&self, index: usize) -> &mut Segment {
        unsafe {
            // SAFETY: The caller ensures that `index is valid, and no other mutable or immutable
            // references exist to this leaf.
            &mut *self.leaves[index].get()
        }
    }

    /// Safely get a mutable reference to a state node.
    pub(crate) fn get_state_mut(&self, index: usize) -> &mut AtomicBool {
        unsafe {
            // SAFETY: The caller ensures that `index` is valid, and no other mutable or immutable
            // references exist to this state node.
            &mut *self.state[index].get()
        }
    }

    /// Reset the structure of the tree so that it can be re-used.
    pub(crate) fn reset(&self) {
        unsafe {
            // SAFETY: This is safe as no mutable references exist during reset.
            // Reset the atomic states used for concurrency.
            for cell in self.state.iter() {
                (*cell.get()).store(true, Ordering::SeqCst);
            }

            // Reset the leaves that were used for building up the tree.
            for cell in self.leaves.iter() {
                (*cell.get()).fill(0);
            }

            // Reset the buffer, ie. level 0 of the tree.
            (*self.buf.get()).fill(0);
        }
    }

    /// Copies data into the `buf` field starting at the given offset.
    ///
    /// # Arguments
    /// - `offset`: The starting position within `buf` where the data should be copied.
    /// - `data`: The data slice to copy into the buffer.
    ///
    /// # Panics
    /// - Panics if the `offset` and `data.len()` exceed the buffer size.
    pub fn copy_to_buf(&self, offset: usize, data: &[u8]) {
        let len = data.len();
        assert!(
            offset + len <= BRANCHES * SEGMENT_SIZE,
            "Attempt to write beyond buffer bounds"
        );

        unsafe {
            // SAFETY: The bounds are checked above, ensuring this write is valid.
            let buffer_ptr = self.buf.get() as *mut u8;
            let slice_ptr = buffer_ptr.add(offset);
            std::ptr::copy_nonoverlapping(data.as_ptr(), slice_ptr, len);
        }
    }
}

/// A [`TreeIterator`] that when given a node's index on **Level 1** of the BMT, will
/// subseqeuntly traverse the tree through to the root.
pub struct TreeIterator {
    /// The BMT for traversal,
    tree_ptr: *const Tree,
    /// The current level (0-indexed) in the BMT for the iterator.
    level: usize,
    /// The current index (0-indexed) of the node on `level` in the BMT.
    index: usize,
}

impl TreeIterator {
    pub fn new(tree: Arc<Tree>, i: usize) -> Self {
        assert!(
            i < BRANCHES / 2,
            "Invalid index, must be less than BMT_BRANCHES / 2"
        );

        // Convert Arc<Tree> into a raw pointer
        let tree_ptr = Arc::as_ptr(&tree);

        Self {
            tree_ptr,
            level: 1, // Start at level 1
            index: i,
        }
    }
}

impl Iterator for TreeIterator {
    type Item = (
        &'static mut Segment,
        (&'static Segment, &'static mut Segment),
        Option<&'static mut AtomicBool>,
        usize,
        usize,
    );

    /// Advances the iterator to the next level and index in the BMT.
    /// Returns:
    /// - A mutable reference to the current node.
    /// - Immutable and mutable references to the left and right children.
    /// - A mutable reference to the parent's atomic state (if applicable).
    /// - The current level and index being processed.
    fn next(&mut self) -> Option<Self::Item> {
        if self.level >= DEPTH {
            return None;
        }

        unsafe {
            // Dereference the raw pointer to Tree
            let tree = &*self.tree_ptr;

            // Determine the left and right children
            let (left, right) = if self.level == 1 {
                // Level 1: Children come from the buffer
                let offset = self.index * (SEGMENT_SIZE * 2);
                (
                    tree.get_buf_segment(offset),
                    tree.get_buf_segment_mut(offset + SEGMENT_SIZE),
                )
            } else {
                // Level >= 2: Children come from the previous level's leaves
                let offset = LEAVES_LEVEL_OFFSETS.get_unchecked(self.level - 2);
                (
                    tree.get_leaf(offset + self.index * 2),
                    tree.get_leaf_mut(offset + self.index * 2 + 1),
                )
            };

            // Determine the current node
            let node =
                tree.get_leaf_mut(LEAVES_LEVEL_OFFSETS.get_unchecked(self.level - 1) + self.index);

            // Determine the parent state
            let parent_state = if self.level == DEPTH - 1 {
                // Root level has no parent
                None
            } else {
                let parent_index = self.index / 2;
                Some(tree.get_state_mut(
                    STATE_LEVEL_OFFSETS.get_unchecked(self.level - 1) + parent_index,
                ))
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

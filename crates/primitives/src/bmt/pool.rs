use std::cell::UnsafeCell;
use std::sync::Arc;

use crate::bmt::HasherBuilder;
use crate::{bmt::tree::Tree, BMT_BRANCHES, HASH_SIZE, SEGMENT_SIZE};

use alloy_primitives::keccak256;
use tokio::sync::{mpsc, Mutex};

use super::{HashError, Hasher, ZERO_SEGMENT};

const SIZE_TO_PARAMS: (usize, usize) = size_to_params(BMT_BRANCHES);
const MAX_SIZE: usize = SIZE_TO_PARAMS.0 * SEGMENT_SIZE;
pub const DEPTH: usize = SIZE_TO_PARAMS.1;

/// Provides a pool of trees used as resource by the BMT Hasher.
/// A tree popped from the pool is guaranteed to have a clean state ready
/// for hashing a new chunk.
#[derive(Debug)]
pub struct Pool {
    pub(crate) config: Arc<PoolConfig>,
    /// Sender used for returning trees back to the pool after use
    pub(crate) sender: mpsc::Sender<UnsafeCell<Tree>>,
    /// Receiver used for receiving trees from the pool when available
    pub(crate) receiver: mpsc::Receiver<UnsafeCell<Tree>>,
}

pub trait PooledHasher {
    async fn get_hasher(&self) -> Result<Hasher, HashError>;
}

impl Pool {
    /// Initialze the pool with a specific capacity
    pub async fn new(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        let config = Arc::new(PoolConfig::default());

        // Pre-fill the Pool
        for _ in 0..capacity {
            let tree = UnsafeCell::new(Tree::new());
            sender.send(tree).await.unwrap();
        }

        Pool {
            config,
            sender,
            receiver,
        }
    }

    /// Consume a tree from the pool asynchronously
    pub(crate) async fn get(&mut self) -> UnsafeCell<Tree> {
        self.receiver.recv().await.expect("Pool is empty")
    }
}

impl PooledHasher for Arc<Mutex<Pool>> {
    async fn get_hasher(&self) -> Result<Hasher, HashError> {
        HasherBuilder::default()
            .with_pool(self.clone())
            .await
            .build()
    }
}

/// Pool configuration for all hashers within the pool
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Lookup table for predictable padding subtrees for all levels
    pub(crate) zero_hashes: [[u8; HASH_SIZE]; DEPTH + 1],
}

impl Default for PoolConfig {
    fn default() -> Self {
        let mut zero_hashes = [[0u8; HASH_SIZE]; DEPTH + 1];
        let mut zeros = ZERO_SEGMENT;

        zero_hashes[0] = zeros;

        for slot in zero_hashes.iter_mut().take(DEPTH + 1).skip(1) {
            zeros = *keccak256([&zeros[..], &zeros[..]].concat());
            *slot = zeros;
        }

        Self { zero_hashes }
    }
}

/// Calculates the depth (number of levels) and segment count in the BMT tree.
/// This is useful for calcualting the zero hash table.
const fn size_to_params(n: usize) -> (usize, usize) {
    let mut c = 2;
    let mut depth = 1;
    while c < n {
        c <<= 1;
        depth += 1;
    }

    (c, depth)
}

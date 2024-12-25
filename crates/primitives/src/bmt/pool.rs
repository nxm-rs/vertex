use crate::bmt::HasherBuilder;
use crate::{bmt::tree::Tree, BMT_BRANCHES, HASH_SIZE, SEGMENT_SIZE};
use std::future::Future;
use std::sync::Arc;

use alloy_primitives::keccak256;
use tokio::sync::{mpsc, Mutex};

use super::{HashError, Hasher, ZERO_SEGMENT};

const SIZE_TO_PARAMS: (usize, usize) = size_to_params(BMT_BRANCHES);
const MAX_SIZE: usize = SIZE_TO_PARAMS.0 * SEGMENT_SIZE;
//const DEPTH: usize = SIZE_TO_PARAMS.1;

/// Provides a pool of trees used as resource by the BMT Hasher.
/// A tree popped from the pool is guaranteed to have a clean state ready
/// for hashing a new chunk.
#[derive(Debug)]
pub struct Pool<const N: usize, const W: usize, const DEPTH: usize>
where
    [(); W * SEGMENT_SIZE]:,
    [(); DEPTH + 1]:,
{
    pub(crate) config: Arc<PoolConfig<W, DEPTH>>,
    /// Sender used for returning trees back to the pool after use
    pub(crate) sender: mpsc::Sender<Arc<Mutex<Tree<W, DEPTH>>>>,
    /// Receiver used for receiving trees from the pool when available
    pub(crate) receiver: Arc<Mutex<mpsc::Receiver<Arc<Mutex<Tree<W, DEPTH>>>>>>,
}

pub trait PooledHasher<const N: usize, const W: usize, const DEPTH: usize>
where
    [(); W * SEGMENT_SIZE]:,
    [(); DEPTH + 1]:,
{
    fn get_hasher(&self) -> impl Future<Output = Result<Hasher<N, W, DEPTH>, HashError>> + Send;
}

impl<const N: usize, const W: usize, const DEPTH: usize> Pool<N, W, DEPTH>
where
    [(); W * SEGMENT_SIZE]:,
    [(); DEPTH + 1]:,
{
    /// Initialze the pool with a specific capacity
    pub async fn new(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        let config = Arc::new(PoolConfig::<W, DEPTH>::default());

        // Pre-fill the Pool
        for _ in 0..capacity {
            let tree = Arc::new(Mutex::new(Tree::<W, DEPTH>::new()));
            sender.send(tree).await.unwrap();
        }

        Pool {
            config,
            sender,
            receiver: Arc::new(Mutex::new(receiver)),
        }
    }

    /// Consume a tree from the pool asynchronously
    pub(crate) async fn get(&self) -> Arc<Mutex<Tree<W, DEPTH>>> {
        self.receiver
            .lock()
            .await
            .recv()
            .await
            .expect("Pool is empty")
    }
}

impl<const N: usize, const W: usize, const DEPTH: usize> PooledHasher<N, W, DEPTH>
    for Arc<Pool<N, W, DEPTH>>
where
    [(); W * SEGMENT_SIZE]:,
    [(); DEPTH + 1]:,
{
    async fn get_hasher(&self) -> Result<Hasher<N, W, DEPTH>, HashError> {
        HasherBuilder::default()
            .with_pool(self.clone())
            .await
            .build()
    }
}

/// Pool configuration for all hashers within the pool
#[derive(Debug, Clone)]
pub struct PoolConfig<const W: usize, const DEPTH: usize>
where
    [(); DEPTH + 1]:,
    [(); W * SEGMENT_SIZE]:,
{
    /// Lookup table for predictable padding subtrees for all levels
    pub(crate) zero_hashes: [[u8; HASH_SIZE]; DEPTH + 1],
}

impl<const W: usize, const DEPTH: usize> Default for PoolConfig<W, DEPTH>
where
    [(); DEPTH + 1]:,
    [(); W * SEGMENT_SIZE]:,
{
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
pub const fn size_to_params(n: usize) -> (usize, usize) {
    let mut c = 2;
    let mut depth = 1;
    while c < n {
        c <<= 1;
        depth += 1;
    }

    (c, depth)
}

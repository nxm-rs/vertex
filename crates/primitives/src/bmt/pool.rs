use crate::bmt::{tree::Tree, HasherBuilder, DEPTH, HASH_SIZE};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::future::Future;
use std::sync::Arc;

use alloy_primitives::keccak256;
use tokio::sync::mpsc;

use super::{HashError, Hasher, Segment, ZERO_SEGMENT};

/// Provides a pool of trees used as resource by the BMT Hasher.
/// A tree popped from the pool is guaranteed to have a clean state ready
/// for hashing a new chunk.
#[derive(Debug)]
pub struct Pool {
    /// Sender used for returning trees back to the pool after use
    pub(crate) sender: mpsc::Sender<Arc<Mutex<Tree>>>,
    /// Receiver used for receiving trees from the pool when available
    pub(crate) receiver: Arc<tokio::sync::Mutex<mpsc::Receiver<Arc<Mutex<Tree>>>>>,
}

pub trait PooledHasher {
    fn get_hasher(&self) -> impl Future<Output = Result<Hasher, HashError>> + Send;
}

impl Pool {
    /// Initialze the pool with a specific capacity
    pub async fn new(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);

        // Pre-fill the Pool
        for _ in 0..capacity {
            let tree = Arc::new(Mutex::new(Tree::new()));
            sender.send(tree).await.unwrap();
        }

        Pool {
            sender,
            receiver: Arc::new(tokio::sync::Mutex::new(receiver)),
        }
    }

    /// Consume a tree from the pool asynchronously
    pub(crate) async fn get(&self) -> Arc<Mutex<Tree>> {
        self.receiver
            .lock()
            .await
            .recv()
            .await
            .expect("Pool is empty")
    }
}

impl PooledHasher for Arc<Pool> {
    async fn get_hasher(&self) -> Result<Hasher, HashError> {
        HasherBuilder::default()
            .with_pool(self.clone())
            .await
            .build()
    }
}

// Lazy initialisation of the zero_hashes lookup table
pub(crate) static ZERO_HASHES: Lazy<[Segment; DEPTH + 1]> = Lazy::new(|| {
    let mut zero_hashes = [[0u8; HASH_SIZE]; DEPTH + 1];
    let mut zeros = ZERO_SEGMENT;

    zero_hashes[0] = zeros;

    for slot in zero_hashes.iter_mut().take(DEPTH + 1).skip(1) {
        zeros = *keccak256([&zeros[..], &zeros[..]].concat());
        *slot = zeros;
    }

    zero_hashes
});

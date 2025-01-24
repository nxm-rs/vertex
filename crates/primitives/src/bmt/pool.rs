use crate::bmt::{HashError, Hasher, HasherBuilder, Tree};
use std::sync::Arc;

use tokio::sync::{
    mpsc::{channel, Receiver, Sender},
    Mutex,
};

/// Provides a pool of trees used as resource by the BMT Hasher.
/// A tree popped from the pool is guaranteed to have a clean state ready
/// for hashing a new chunk.
#[derive(Debug)]
pub struct Pool {
    /// Sender used for returning trees back to the pool after use
    pub(crate) sender: Sender<Arc<Tree>>,
    /// Receiver used for receiving trees from the pool when available
    pub(crate) receiver: Arc<Mutex<Receiver<Arc<Tree>>>>,
}

/// Defines a resource pool of BMTs that are available to be used by Hashers.
pub trait PooledHasher {
    /// Get a [`Hasher`] from the resource pool.
    fn get_hasher(&self) -> impl std::future::Future<Output = Result<Hasher, HashError>> + Send;
}

impl Pool {
    /// Initialze the pool with a specific capacity
    pub async fn new(capacity: usize) -> Self {
        let (sender, receiver) = channel(capacity);

        // Pre-fill the Pool
        for _ in 0..capacity {
            let tree = Arc::new(Tree::new());
            sender.send(tree).await.unwrap();
        }

        Pool {
            sender,
            receiver: Arc::new(Mutex::new(receiver)),
        }
    }

    /// Consume a tree from the pool asynchronously
    pub(crate) async fn get(&self) -> Arc<Tree> {
        self.receiver
            .lock()
            .await
            .recv()
            .await
            .expect("Pool is empty")
    }

    /// Return a tree back to the pool asynchronously. We make sure to reset the tree prior to
    /// sending back to the pool to make sure it's in a condition for consumption when requested.
    pub(crate) async fn put(&self, tree: Arc<Tree>) {
        tree.reset();
        if let Err(e) = self.sender.send(tree).await {
            eprintln!("Failed to return tree to pool: {:?}", e);
        }
    }
}

impl PooledHasher for Arc<Pool> {
    async fn get_hasher(&self) -> Result<Hasher, HashError> {
        let tree = self.get().await;
        HasherBuilder::default()
            .with_tree(tree)
            .with_pool(self.clone())
            .build()
    }
}

use crate::bmt::{tree::Tree, HasherBuilder};
use std::future::Future;
use std::sync::Arc;

use tokio::sync::mpsc;

use super::{HashError, Hasher};

/// Provides a pool of trees used as resource by the BMT Hasher.
/// A tree popped from the pool is guaranteed to have a clean state ready
/// for hashing a new chunk.
#[derive(Debug)]
pub struct Pool {
    /// Sender used for returning trees back to the pool after use
    pub(crate) sender: mpsc::Sender<Arc<Tree>>,
    /// Receiver used for receiving trees from the pool when available
    pub(crate) receiver: Arc<tokio::sync::Mutex<mpsc::Receiver<Arc<Tree>>>>,
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
            let tree = Arc::new(Tree::new());
            sender.send(tree).await.unwrap();
        }

        Pool {
            sender,
            receiver: Arc::new(tokio::sync::Mutex::new(receiver)),
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

    /// Return a tree back to the pool asynchronously
    pub(crate) async fn put(&self, tree: Arc<Tree>) {
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

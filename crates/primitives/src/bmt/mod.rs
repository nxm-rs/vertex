use crate::bmt::tree::Tree;
use alloy_primitives::Keccak256;
use anyhow::Result;
use futures::future::join_all;
use std::sync::{atomic::Ordering, Arc};
use thiserror::Error;
use tree::TreeIterator;

use crate::{CHUNK_SIZE, SEGMENT_SIZE, SPAN_SIZE};

pub(crate) type Span = [u8; SPAN_SIZE];
pub(crate) type Segment = [u8; SEGMENT_SIZE];

pub mod pool;
//pub mod proof;
pub mod reference;
pub mod tree;
mod zero_hashes;
use pool::Pool;
use zero_hashes::ZERO_HASHES;

const SEGMENT_PAIR_SIZE: usize = 2 * SEGMENT_SIZE;

const ZERO_SPAN: Span = [0u8; SPAN_SIZE];

#[derive(Debug)]
pub struct Hasher {
    pool: Option<Arc<Pool>>,
    tree: Arc<Tree>,
    size: usize,
    pos: usize,
    span: Span,
}

unsafe impl Send for Hasher {}
unsafe impl Sync for Hasher {}

#[derive(Default)]
pub struct HasherBuilder {
    pool: Option<Arc<Pool>>,
    tree: Option<Arc<Tree>>,
}

impl HasherBuilder {
    /// Create a default builder whereby all options are set to `None`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Populate the builder with configuration from the respective pool..
    pub fn with_pool(mut self, pool: Arc<Pool>) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Use the respective [`Tree`] for building the BMT. This allows for resource reuse and
    /// prevents repetitive allocations.
    pub fn with_tree(mut self, tree: Arc<Tree>) -> Self {
        self.tree = Some(tree);
        self
    }

    /// Given the state of the builder, construct a [`Hasher`].
    pub fn build(self) -> Result<Hasher, HashError> {
        let tree = self.tree.unwrap_or(Arc::new(Tree::new()));

        Ok(Hasher {
            tree,
            size: 0,
            pos: 0,
            span: ZERO_SPAN,
            pool: self.pool,
        })
    }
}

#[derive(Error, Debug)]
pub enum HashError {
    #[error("Invalid length: {0}")]
    InvalidLength(u64),
}

impl Hasher {
    pub fn hash(&mut self, output: &mut [u8]) {
        if self.size == 0 {
            return self.root_hash(ZERO_HASHES.last().unwrap(), output);
        }

        // write the last section with final flag set to true
        self.root_hash(
            &Self::process_segment_pair(self.tree.clone(), self.pos, true).unwrap(),
            output,
        );

        self.reset();
    }

    /// Write calls sequentially add to the buffer to be hashed, with every full segment calls
    /// process_segment_pair in another thread.
    pub async fn write(&mut self, data: &[u8]) -> Result<usize> {
        let mut len = data.len();
        let max_val = CHUNK_SIZE - self.size;

        if len > max_val {
            len = max_val;
        }

        // Copy data into the internal buffer
        unsafe {
            let buffer_ptr = self.tree.buf.as_ptr() as *mut u8;
            let slice_ptr = buffer_ptr.add(self.size);
            std::ptr::copy_nonoverlapping(data.as_ptr(), slice_ptr, len);
        }

        // Calculate segment properties
        let from = self.size / SEGMENT_PAIR_SIZE;
        self.size += len;
        let mut to = self.size / SEGMENT_PAIR_SIZE;

        if len % SEGMENT_PAIR_SIZE == 0 && len > 1 {
            to -= 1;
        }
        self.pos = to;

        let mut handlers = Vec::new();
        for i in from..to {
            let tree = self.tree.clone();
            let handler = tokio::spawn(async move {
                Self::process_segment_pair(tree, i, false);
            });
            handlers.push(handler);
        }

        let _ = join_all(handlers).await;
        Ok(len)
    }

    /// Given a [`Hasher`] instance, reset it for further use.
    pub fn reset(&mut self) {
        self.tree.reset();
        (self.pos, self.size, self.span) = (0, 0, ZERO_SPAN);
    }

    /// Set the header bytes of BMT hash by copying the first 8 bytes of the argument
    pub fn set_header_bytes(&mut self, header: &[u8]) -> Result<(), HashError> {
        if header.len() == SPAN_SIZE {
            self.span.copy_from_slice(&header[0..SPAN_SIZE]);
            Ok(())
        } else {
            Err(HashError::InvalidLength(header.len().try_into().unwrap()))
        }
    }

    /// Set the header bytes of BMT hash by the little-endian encoded u64.
    pub fn set_header_u64(&mut self, header: u64) -> Result<(), HashError> {
        if header <= CHUNK_SIZE as u64 {
            self.span = header.to_le_bytes();
            Ok(())
        } else {
            Err(HashError::InvalidLength(header))
        }
    }

    // Writes the hash of the i-th segment pair into level 1 node of the BMT tree.
    fn process_segment_pair(tree: Arc<Tree>, i: usize, is_final: bool) -> Option<Segment> {
        let tree_ref = unsafe { &*Arc::as_ptr(&tree) };
        let tree_iterator = TreeIterator::new(tree_ref, i);

        for (current_node, (left, right), state, level, _) in tree_iterator {
            let right = if is_final && right == ZERO_HASHES[0] {
                &ZERO_HASHES[level - 1]
            } else {
                right
            };

            let mut hasher = Keccak256::new();
            hasher.update(left);
            hasher.update(right);
            hasher.finalize_into(unsafe { &mut *current_node });

            if is_final && state.is_none() {
                return Some(unsafe { *current_node });
            } else if let Some(state_ptr) = state {
                let prev_state = unsafe { (*state_ptr).fetch_not(Ordering::SeqCst) };
                if prev_state && !is_final {
                    return None;
                }
            }
        }
        // Only to satisfy the compiler
        None
    }

    fn root_hash(&self, last: &[u8], output: &mut [u8]) {
        let mut hasher = Keccak256::new();
        hasher.update(self.span);
        hasher.update(last);

        hasher.finalize_into(output)
    }
}

impl Drop for Hasher {
    fn drop(&mut self) {
        if let Some(pool) = &self.pool {
            let pool = pool.clone();
            let tree = self.tree.clone();
            tokio::spawn(async move {
                tree.reset();
                pool.put(tree).await
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::BMT_BRANCHES;
    use alloy_primitives::b256;
    use futures::future::join_all;
    use pool::{Pool, PooledHasher};
    use rand::{rngs::StdRng, Rng, RngCore, SeedableRng};
    use reference::RefHasher;

    use super::*;

    const POOL_SIZE: usize = 16;

    fn rand_data<const LENGTH: usize>() -> (Box<dyn RngCore>, Vec<u8>, String) {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs();
        let mut rng = StdRng::seed_from_u64(seed);
        let mut data = vec![0u8; LENGTH];
        rng.fill(&mut data[..]);

        (Box::new(rng), data, format!("seed: {}", seed))
    }

    fn ref_hash(data: &[u8]) -> Segment {
        let ref_bmt: RefHasher<BMT_BRANCHES> = RefHasher::new();
        let ref_no_metahash = ref_bmt.hash(data);

        let mut hasher = Keccak256::new();
        hasher.update((data.len() as u64).to_le_bytes());
        hasher.update(ref_no_metahash.as_slice());

        *hasher.finalize()
    }

    async fn sync_hash(hasher: Arc<tokio::sync::Mutex<Hasher>>, data: &[u8]) -> Segment {
        let mut hasher = hasher.lock().await;

        hasher.set_header_u64(data.len() as u64).unwrap();
        hasher.write(data).await.unwrap();
        let mut segment: Segment = [0u8; 32];
        hasher.hash(segment.as_mut_slice());

        segment
    }

    // Test correctness by comparing against the reference implementation
    async fn test_hasher_correctness(
        hasher: Arc<tokio::sync::Mutex<Hasher>>,
        data: &[u8],
        msg: Option<String>,
    ) {
        let exp_hash = ref_hash(data);
        let res_hash = sync_hash(hasher, data).await;

        assert_eq!(
            exp_hash, res_hash,
            "Hash mismatch: expected {:?} got {:?} with {:?}",
            exp_hash, res_hash, msg
        );
    }

    #[tokio::test]
    async fn test_concurrent_simple() {
        let data: [u8; 3] = [1, 2, 3];

        let pool = Arc::new(Pool::new(1).await);
        let mut hasher = pool.get_hasher().await.unwrap();
        hasher.set_header_u64(data.len() as u64).unwrap();
        hasher.write(&data).await.unwrap();
        let mut res_hash: Segment = [0u8; 32];
        hasher.hash(&mut res_hash);

        assert_eq!(
            res_hash,
            b256!("ca6357a08e317d15ec560fef34e4c45f8f19f01c372aa70f1da72bfa7f1a4338")
        );
    }

    #[tokio::test]
    async fn test_concurrent_fullsize() {
        let pool = Arc::new(Pool::new(1).await);
        let hasher = Arc::new(tokio::sync::Mutex::new(pool.get_hasher().await.unwrap()));
        let (_, data, msg) = rand_data::<CHUNK_SIZE>();
        test_hasher_correctness(hasher, &data, Some(msg)).await;
    }

    #[tokio::test]
    async fn test_hasher_empty_data() {
        let pool = Arc::new(Pool::new(1).await);
        let hasher = Arc::new(tokio::sync::Mutex::new(pool.get_hasher().await.unwrap()));

        test_hasher_correctness(hasher, &[], None).await;
    }

    #[tokio::test]
    async fn test_sync_hasher_correctness() {
        let pool = Arc::new(Pool::new(1).await);
        let (mut rng, data, msg) = rand_data::<CHUNK_SIZE>();

        let mut start = 0;
        while start < data.len() {
            let hasher = Arc::new(tokio::sync::Mutex::new(pool.get_hasher().await.unwrap()));
            test_hasher_correctness(hasher, &data[..start], Some(msg.clone())).await;
            start += 1 + rng.gen_range(0..=5);
        }
    }

    #[tokio::test]
    async fn test_hasher_reuse() {
        let pool = Arc::new(Pool::new(POOL_SIZE).await);
        let hasher = Arc::new(tokio::sync::Mutex::new(pool.get_hasher().await.unwrap()));

        for _ in 0..100 {
            let test_data: Vec<u8> = (0..CHUNK_SIZE).map(|_| rand::random::<u8>()).collect();
            let test_length = rand::random::<usize>() % CHUNK_SIZE;
            test_hasher_correctness(hasher.clone(), &test_data[..test_length], None).await;
        }
    }

    #[tokio::test]
    async fn test_concurrent_use() {
        let pool = Arc::new(Pool::new(POOL_SIZE).await);
        let (mut rng, data, msg) = rand_data::<CHUNK_SIZE>();
        let num_tasks = 100;

        let handles: Vec<_> = (0..num_tasks)
            .map(|_| {
                let pool = pool.clone();
                let data = data.clone();
                let len = rng.gen_range(0..=CHUNK_SIZE);
                let msg = msg.clone();
                tokio::spawn(async move {
                    let hasher =
                        Arc::new(tokio::sync::Mutex::new(pool.get_hasher().await.unwrap()));
                    test_hasher_correctness(hasher, &data[..len], Some(msg)).await
                })
            })
            .collect();

        join_all(handles).await;
    }
}

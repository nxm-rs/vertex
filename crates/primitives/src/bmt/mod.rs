use crate::bmt::tree::{is_left, Node, Tree};
use alloy_primitives::keccak256;
use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::mpsc;

use crate::{CHUNK_SIZE, HASH_SIZE, SEGMENT_SIZE, SPAN_SIZE};

pub(crate) type Span = [u8; SPAN_SIZE];
pub(crate) type Segment = [u8; SEGMENT_SIZE];

pub mod pool;
pub mod proof;
pub mod reference;
pub mod tree;
use pool::{Pool, ZERO_HASHES};

const SEGMENT_PAIR_SIZE: usize = 2 * SEGMENT_SIZE;

const ZERO_SPAN: Span = [0u8; SPAN_SIZE];
const ZERO_SEGMENT: Segment = [0u8; SEGMENT_SIZE];

pub(crate) const DEPTH: usize = 7;

#[derive(Debug)]
pub struct Hasher {
    bmt: Arc<Mutex<Tree>>,
    size: usize,
    pos: usize,
    span: Span,
    // Channels
    result_tx: Option<mpsc::Sender<Segment>>,
    result_rx: Option<mpsc::Receiver<Segment>>,
    pool_tx: Option<mpsc::Sender<Arc<Mutex<Tree>>>>,
}

#[derive(Default)]
pub struct HasherBuilder {
    bmt: Option<Arc<Mutex<Tree>>>,
    pool_tx: Option<mpsc::Sender<Arc<Mutex<Tree>>>>,
}

impl HasherBuilder {
    /// Create a default builder whereby all options are set to `None`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Populate the builder with configuration from the respective pool..
    pub async fn with_pool(mut self, pool: Arc<Pool>) -> Self {
        self.bmt = Some(pool.get().await);
        self.pool_tx = Some(pool.sender.clone());

        self
    }

    /// Use the respective [`Tree`] for building the BMT. This allows for resource reuse and
    /// prevents repetitive allocations.
    pub fn with_bmt(mut self, bmt: Arc<Mutex<Tree>>) -> Self {
        self.bmt = Some(bmt);
        self
    }

    /// When the [`Hasher`] drops, it will return the BMT resource back to the pool using this
    /// channel.
    pub fn with_pool_tx(mut self, pool_tx: mpsc::Sender<Arc<Mutex<Tree>>>) -> Self {
        self.pool_tx = Some(pool_tx);
        self
    }

    /// Given the state of the builder, construct a [`Hasher`].
    pub fn build(self) -> Result<Hasher, HashError> {
        let bmt = self.bmt.unwrap_or(Arc::new(Mutex::new(Tree::new())));
        let (result_tx, result_rx) = mpsc::channel::<Segment>(1);

        Ok(Hasher {
            bmt,
            size: 0,
            pos: 0,
            span: ZERO_SPAN,
            result_tx: Some(result_tx),
            result_rx: Some(result_rx),
            pool_tx: self.pool_tx,
        })
    }
}

#[derive(Error, Debug)]
pub enum HashError {
    #[error("MissingPoolConfig")]
    MissingConfig,
    #[error("Missing BMT")]
    MissingBmt,
    #[error("Missing Pooltx")]
    MissingPoolTx,
    #[error("Invalid length: {0}")]
    InvalidLength(usize),
    #[error("Receiving channel failed")]
    ReceivedChannelFail,
}

impl Hasher {
    pub async fn hash(&mut self) -> Result<Segment> {
        if self.size == 0 {
            return Ok(self.root_hash(ZERO_HASHES.last().unwrap()));
        }

        // Fill the remaining buffer with zeroes
        self.bmt.lock().buffer[self.size..].fill(0);

        // write the last section with final flag set to true
        process_segment_pair(self.bmt.clone(), self.pos, true, self.result_tx.clone()).await;

        match self.result_rx.take() {
            Some(mut rx) => {
                let result = rx.recv().await.unwrap();
                Ok(self.root_hash(&result))
            }
            None => Err(anyhow!("Receiving channel already used")),
        }
    }

    /// Write calls sequentially add to the buffer to be hashed, with every full segment calls
    /// process_segment_pair in another thread.
    pub fn write(&mut self, data: &[u8]) -> Result<usize> {
        let mut len = data.len();
        let max_val = CHUNK_SIZE - self.size;

        if len > max_val {
            len = max_val;
        }

        // Copy data into the internal buffer
        let mut bmt = self.bmt.lock();
        bmt.buffer[self.size..self.size + len].copy_from_slice(&data[..len]);
        drop(bmt);

        // Calculate segment properties
        let from = self.size / SEGMENT_PAIR_SIZE;
        self.size += len;
        let mut to = self.size / SEGMENT_PAIR_SIZE;

        if len % SEGMENT_PAIR_SIZE == 0 && len > 1 {
            to -= 1;
        }
        self.pos = to;

        for i in from..to {
            let bmt = self.bmt.clone();
            let result_tx = self.result_tx.clone();
            tokio::spawn(async move {
                process_segment_pair(bmt, i, false, result_tx).await;
            });
        }

        Ok(len)
    }

    /// Given a [`Hasher`] instance, reset it for further use.
    pub fn reset(&mut self) {
        (self.pos, self.size, self.span) = (0, 0, ZERO_SPAN);

        let (tx, rx) = mpsc::channel::<Segment>(1);
        self.result_tx = Some(tx);
        self.result_rx = Some(rx);
    }

    /// Set the header bytes of BMT hash by copying the first 8 bytes of the argument
    pub fn set_header_bytes(&mut self, header: &[u8]) -> Result<(), HashError> {
        let length = header.len();
        match length == SPAN_SIZE {
            true => {
                self.span.copy_from_slice(&header[0..SPAN_SIZE]);
                Ok(())
            }
            false => Err(HashError::InvalidLength(length)),
        }
    }

    /// Set the header bytes of BMT hash by the little-endian encoded u64.
    pub fn set_header_u64(&mut self, header: u64) {
        self.span = length_to_span(header);
    }

    fn root_hash(&self, last: &[u8]) -> Segment {
        let mut input = [0u8; SPAN_SIZE + HASH_SIZE];

        input[..SPAN_SIZE].copy_from_slice(&self.span[..]);
        input[SPAN_SIZE..(SPAN_SIZE + HASH_SIZE)].copy_from_slice(last);

        *keccak256(input)
    }
}

// Writes the hash of the i-th segment pair into level 1 node of the BMT tree.
async fn process_segment_pair(
    tree: Arc<Mutex<Tree>>,
    i: usize,
    is_final: bool,
    result_tx: Option<mpsc::Sender<Segment>>,
) {
    let offset = i * SEGMENT_PAIR_SIZE;

    // Select the leaf node for the segment pair
    let (n, segment_pair_hash) = {
        let tree = tree.lock();
        let segment_pair_hash = keccak256(&tree.buffer[offset..offset + SEGMENT_PAIR_SIZE]);
        let n = tree.leaves[i].lock();

        (n.parent().clone(), segment_pair_hash)
    };

    match is_final {
        true => write_final_node(i, n, Some(*segment_pair_hash), result_tx).await,
        false => write_node(i, n, *segment_pair_hash, result_tx).await,
    }
}

async fn send_segment(sender: Option<mpsc::Sender<Segment>>, segment: Segment) {
    if let Some(tx) = &sender {
        let tx = tx.clone();
        tokio::spawn(async move {
            if let Err(_e) = tx.send(segment).await {
                todo!("Add error tracing here");
            }
        });
    }
}

/// Pushes the data to the node.
/// If it is the first of 2 sisters written, the routine terminates.
/// If it is the second, it calculates the hash and writes it to the
/// parent node recursively.
#[inline(always)]
async fn write_node(
    i: usize,
    mut node: Option<Arc<Mutex<Node>>>,
    mut segment: Segment,
    result_tx: Option<mpsc::Sender<Segment>>,
) {
    let mut level = 1;
    while let Some(node_ref) = node {
        let mut node_mut = node_ref.lock();
        node_mut.set(is_left(level, i), segment);

        // if the opposite segment isn't filled, waiting on the other thread so exit.
        if node_mut.toggle() {
            return;
        }

        segment = node_mut.hash_segment();
        node = node_mut.parent();
        level += 1;
    }

    // Reached the root of the BMT - send it!
    send_segment(result_tx, segment).await;
}

/// Follow the path starting from the final data segment to the BMT root via parents.
/// For unbalanced trees it fills in the missing right sister nodes using the pool's lookup
/// table for BMT subtree root hashes for all-zero sections.
/// Otherwise behaves like `write_node`.
#[inline(always)]
async fn write_final_node(
    i: usize,
    mut node: Option<Arc<Mutex<Node>>>,
    mut segment: Option<Segment>,
    result_tx: Option<mpsc::Sender<Segment>>,
) {
    const LEFT: bool = true;
    const RIGHT: bool = false;

    let mut level: usize = 1;
    while let Some(node_ref) = node {
        let mut node_mut = node_ref.lock();

        let no_hash = match is_left(level, i) {
            // Coming from left sister branch
            // When the final segment's path is going via left child node we include an
            // all-zero subtree hash for the right level and toggle the node.
            true => {
                node_mut.set(RIGHT, ZERO_HASHES[level]);

                if let Some(seg) = segment {
                    // If a left final node carries a hash, it must be the first (and only
                    // thread), so the toggle is already in passive state. No need to call
                    // yet thread needs to carry on pushing hash to parent.
                    node_mut.set(LEFT, seg);

                    false
                } else {
                    // If the first thread then propagate None and calculate no hash
                    node_mut.toggle()
                }
            }
            false => {
                if let Some(seg) = segment {
                    // If hash was pushed from right child node, write right segment change
                    // state
                    node_mut.set(RIGHT, seg);
                    // If toggle is true, we arrived first so no hashing just push None to
                    // parent.
                    node_mut.toggle()
                } else {
                    // If sister is None, then thread arrived first at previous node and
                    // here there will be two so no need to do anything and keep sister =
                    // None for parent.
                    true
                }
            }
        };

        segment = if no_hash {
            None
        } else {
            Some(node_mut.hash_segment())
        };

        node = node_mut.parent();
        level += 1;
    }

    if let Some(seg) = segment {
        send_segment(result_tx, seg).await;
    }
}

impl Drop for Hasher {
    fn drop(&mut self) {
        if let Some(tx) = &self.pool_tx {
            let tx = tx.clone();
            let value = self.bmt.clone();
            tokio::spawn(async move {
                if let Err(e) = tx.send(value).await {
                    eprintln!("Error sending data through the async channel: {:?}", e);
                }
            });
        }
    }
}

/// Creates a binary data span size representation - required for calcualting the BMT hash
fn length_to_span(length: u64) -> Span {
    length.to_le_bytes()
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

        *keccak256(
            [
                length_to_span(data.len().try_into().unwrap()).as_slice(),
                ref_no_metahash.as_slice(),
            ]
            .concat(),
        )
    }

    async fn sync_hash(hasher: Arc<tokio::sync::Mutex<Hasher>>, data: &[u8]) -> Segment {
        let mut hasher = hasher.lock().await;
        hasher.reset();

        hasher.set_header_u64(data.len().try_into().unwrap());
        hasher.write(data).unwrap();
        hasher.hash().await.unwrap()
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
        let hasher = Arc::new(Mutex::new(pool.get_hasher().await.unwrap()));
        let mut hasher = hasher.lock();
        hasher.set_header_u64(data.len().try_into().unwrap());
        hasher.write(&data).unwrap();
        let res_hash = hasher.hash().await.unwrap();

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
    async fn test_bmt_concurrent_use() {
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

    // TODO: Decide whether or not to implement AsyncWrite or Write trait
}

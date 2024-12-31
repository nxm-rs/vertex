use crate::bmt::tree::{Node, Tree};
use alloy_primitives::keccak256;
use anyhow::{anyhow, Result};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{mpsc, Mutex};

use crate::{HASH_SIZE, SEGMENT_SIZE, SPAN_SIZE};

pub(crate) type Span = [u8; SPAN_SIZE];
pub(crate) type Segment = [u8; SEGMENT_SIZE];

pub mod pool;
pub mod reference;
pub mod tree;
use pool::{Pool, ZERO_HASHES};

const SEGMENT_PAIR_SIZE: usize = 2 * SEGMENT_SIZE;

const ZERO_SPAN: Span = [0u8; SPAN_SIZE];
const ZERO_SEGMENT: Segment = [0u8; SEGMENT_SIZE];

pub(crate) type SegmentPair = [u8; SEGMENT_PAIR_SIZE];
const ZERO_SEGMENT_PAIR: SegmentPair = [0u8; SEGMENT_PAIR_SIZE];

#[derive(Debug)]
pub struct Hasher<const N: usize, const W: usize, const DEPTH: usize>
where
    [(); W * SEGMENT_SIZE]:,
    [(); DEPTH + 1]:,
{
    bmt: Arc<Mutex<Tree<W, DEPTH>>>,
    size: usize,
    pos: usize,
    span: Span,
    // Channels
    result_tx: Option<mpsc::Sender<Segment>>,
    result_rx: Option<mpsc::Receiver<Segment>>,
    pool_tx: Option<mpsc::Sender<Arc<Mutex<Tree<W, DEPTH>>>>>,
}

#[derive(Default)]
pub struct HasherBuilder<const N: usize, const W: usize, const DEPTH: usize>
where
    [(); W * SEGMENT_SIZE]:,
    [(); DEPTH + 1]:,
{
    bmt: Option<Arc<Mutex<Tree<W, DEPTH>>>>,
    pool_tx: Option<mpsc::Sender<Arc<Mutex<Tree<W, DEPTH>>>>>,
}

impl<const N: usize, const W: usize, const DEPTH: usize> HasherBuilder<N, W, DEPTH>
where
    [(); W * SEGMENT_SIZE]:,
    [(); DEPTH + 1]:,
{
    /// Create a default builder whereby all options are set to `None`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Populate the builder with configuration from the respective pool..
    pub async fn with_pool(mut self, pool: Arc<Pool<W, DEPTH>>) -> Self {
        self.bmt = Some(pool.get().await);
        self.pool_tx = Some(pool.sender.clone());

        self
    }

    /// Use the respective [`Tree`] for building the BMT. This allows for resource reuse and
    /// prevents repetitive allocations.
    pub fn with_bmt(mut self, bmt: Arc<Mutex<Tree<W, DEPTH>>>) -> Self {
        self.bmt = Some(bmt);
        self
    }

    /// When the [`Hasher`] drops, it will return the BMT resource back to the pool using this
    /// channel.
    pub fn with_pool_tx(mut self, pool_tx: mpsc::Sender<Arc<Mutex<Tree<W, DEPTH>>>>) -> Self {
        self.pool_tx = Some(pool_tx);
        self
    }

    /// Given the state of the builder, construct a [`Hasher`].
    pub fn build(self) -> Result<Hasher<N, W, DEPTH>, HashError> {
        let config = self.config.unwrap_or(Arc::new(PoolConfig::default()));
        let bmt = self.bmt.unwrap_or(Arc::new(Mutex::new(Tree::new())));
        let (result_tx, result_rx) = mpsc::channel::<Segment>(1);

        Ok(Hasher {
            config,
            bmt,
            size: 0,
            // todo should be able trim out this max_size as this sohuld be equal to W
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

impl<const N: usize, const W: usize, const DEPTH: usize> Hasher<N, W, DEPTH>
where
    [(); W * SEGMENT_SIZE]:,
    [(); DEPTH + 1]:,
{
    pub async fn hash(&mut self) -> Result<Segment> {
        if self.size == 0 {
            return Ok(self.root_hash(&ZERO_HASHES[DEPTH]));
        }

        // Fill the remaining buffer with zeroes
        let mut bmt = self.bmt.lock().await;
        bmt.buffer[self.size..].fill(0);
        drop(bmt);

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
    pub async fn write(&mut self, data: &[u8]) -> Result<usize> {
        let mut len = data.len();
        let max_val = W * SEGMENT_SIZE - self.size;

        if len > max_val {
            len = max_val;
        }

        // Copy data into the internal buffer
        let mut bmt = self.bmt.lock().await;
        bmt.buffer[self.size..self.size + len].copy_from_slice(&data[..len]);

        // Calculate segment properties
        let from = self.size / SEGMENT_PAIR_SIZE;
        self.size += len;
        let mut to = self.size / SEGMENT_PAIR_SIZE;

        println!("to: {}", to);

        if len == max_val {
            to -= 1;
        }
        self.pos = to;

        println!("from: {}, to: {}, size: {}", from, to, self.size);

        for i in from..to {
            let config = self.config.clone();
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
async fn process_segment_pair<const W: usize, const DEPTH: usize>(
    tree: Arc<Mutex<Tree<W, DEPTH>>>,
    i: usize,
    is_final: bool,
    result_tx: Option<mpsc::Sender<Segment>>,
) where
    [(); W * SEGMENT_SIZE]:,
    [(); DEPTH + 1]:,
{
    let offset = i * SEGMENT_PAIR_SIZE;
    let level = 1;

    // Select the leaf node for the segment pair
    let (n, is_left, segment_pair_hash) = {
        let tree = tree.lock().await;
        let segment_pair_hash = keccak256(&tree.buffer[offset..offset + SEGMENT_PAIR_SIZE]);
        let n = tree.leaves[i].lock().await;

        (n.parent(), n.is_left, segment_pair_hash)
    };

    match is_final {
        true => write_final_node(level, n, is_left, Some(*segment_pair_hash), result_tx).await,
        false => write_node(n, is_left, *segment_pair_hash, result_tx).await,
    }
}

async fn send_segment(sender: Option<mpsc::Sender<Segment>>, segment: Segment) {
    if let Some(tx) = &sender {
        let tx = tx.clone();
        if let Err(_e) = tx.send(segment).await {
            todo!("Add error tracing here");
        }
    }
}

/// Pushes the data to the node.
/// If it is the first of 2 sisters written, the routine terminates.
/// If it is the second, it calculates the hash and writes it to the
/// parent node recursively.
async fn write_node(
    mut node: Option<Arc<Mutex<Node>>>,
    mut is_left: bool,
    mut segment: [u8; HASH_SIZE],
    result_tx: Option<mpsc::Sender<Segment>>,
) {
    while let Some(node_ref) = node {
        let mut node_mut = node_ref.lock().await;
        node_mut.set(is_left, segment);

        // if the opposite segment isn't filled, waiting on the other thread so exit.
        if node_mut.segment(!is_left).is_none() {
            return;
        }

        segment = node_mut.hash_segment();
        is_left = node_mut.is_left;
        node = node_mut.parent();
    }

    // Reached the root of the BMT - send it!
    send_segment(result_tx, segment).await;
}

/// Follow the path starting from the final data segment to the BMT root via parents.
/// For unbalanced trees it fills in the missing right sister nodes using the pool's lookup
/// table for BMT subtree root hashes for all-zero sections.
/// Otherwise behaves like `write_node`.
async fn write_final_node<const W: usize, const DEPTH: usize>(
    mut level: usize,
    mut node: Option<Arc<Mutex<Node>>>,
    mut is_left: bool,
    mut segment: Option<[u8; HASH_SIZE]>,
    result_tx: Option<mpsc::Sender<Segment>>,
) where
    [(); W * SEGMENT_SIZE]:,
    [(); DEPTH + 1]:,
{
    while let Some(node_ref) = node {
        let mut node_mut = node_ref.lock().await;

        let no_hash = match is_left {
            // Coming from left sister branch
            // When the final segment's path is going via left child node we include an
            // all-zero subtree hash for the right level and toggle the node.
            true => {
                node_mut.set(false, ZERO_HASHES[level]);

                if let Some(seg) = segment {
                    // If a left final node carries a hash, it must be the first (and only
                    // thread), so the toggle is already in passive state. No need to call
                    // yet thread needs to carry on pushing hash to parent.
                    node_mut.set(true, seg);

                    false
                } else {
                    // If the first thread then propagate None and calculate no hash
                    true
                }
            }
            false => {
                if let Some(seg) = segment {
                    // If hash was pushed from right child node, write right segment change
                    // state
                    node_mut.set(false, seg);
                    // If toggle is true, we arrived first so no hashing just push None to
                    // parent.
                    node_mut.segment(true).is_none()
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

        is_left = node_mut.is_left;
        node = node_mut.parent();
        level += 1;
    }

    if let Some(seg) = segment {
        send_segment(result_tx, seg).await;
    }
}

//async fn write_final_node<const W: usize, const DEPTH: usize>(
//    mut level: usize,
//    mut node: Option<Arc<Mutex<Node>>>,
//    mut is_left: bool,
//    mut segment: Option<[u8; HASH_SIZE]>,
//    result_tx: Option<mpsc::Sender<[u8; 32]>>,
//    config: Arc<PoolConfig<W, DEPTH>>,
//) where
//    [(); W * SEGMENT_SIZE]:,
//    [(); DEPTH + 1]:,
//{
//    while let Some(node_ref) = node {
//        let mut node_mut = node_ref.lock().await;
//
//        if let Some(seg) = segment {
//            node_mut.set(is_left, seg);
//        } else if is_left {
//            node_mut.set(false, config.zero_hashes[level]);
//        }
//
//        if node_mut.segment(!is_left).is_none() {
//            break;
//        }
//
//        segment = Some(node_mut.hash_segment());
//        is_left = node_mut.is_left;
//        node = node_mut.parent();
//        level += 1;
//    }
//
//    if let Some(seg) = segment {
//        send_segment(result_tx, seg).await;
//    }
//}

impl<const N: usize, const W: usize, const DEPTH: usize> Drop for Hasher<N, W, DEPTH>
where
    [(); W * SEGMENT_SIZE]:,
    [(); DEPTH + 1]:,
{
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

///// Returns length from span
//fn length_from_span(span: Span) -> u64 {
//    u64::from_le_bytes(span)
//}

#[cfg(test)]
mod tests {
    use alloy_primitives::b256;
    use pool::{size_to_params, Pool, PooledHasher};
    use reference::RefHasher;

    use super::*;
    use paste::paste;

    const POOL_SIZE: usize = 16;

    fn ref_hash<const N: usize>(data: &[u8]) -> Segment {
        let ref_bmt: RefHasher<N> = RefHasher::new();
        let ref_no_metahash = ref_bmt.hash(data);

        *keccak256(
            [
                length_to_span(data.len().try_into().unwrap()).as_slice(),
                ref_no_metahash.as_slice(),
            ]
            .concat(),
        )
    }

    async fn sync_hash<const N: usize, const W: usize, const DEPTH: usize>(
        hasher: Arc<Mutex<Hasher<N, W, DEPTH>>>,
        data: &[u8],
    ) -> Segment
    where
        [(); W * SEGMENT_SIZE]:,
        [(); DEPTH + 1]:,
    {
        println!("Running sync_hash with N: {} W: {} DEPTH: {}", N, W, DEPTH);
        println!("Data length: {}", data.len());
        println!("Data: {:?}", data);
        let mut hasher = hasher.lock().await;
        hasher.reset();

        hasher.set_header_u64(data.len().try_into().unwrap());
        hasher.write(data).await.unwrap();
        hasher.hash().await.unwrap()
    }

    #[test]
    fn test_zerohashes() {
        for i in 0..ZERO_HASHES.len() {
            println!("Zero hash {}: {:?}", i, ZERO_HASHES[i]);
        }
    }

    #[tokio::test]
    async fn test_concurrent_simple() {
        let data: [u8; 3] = [1, 2, 3];

        let pool = Arc::new(Pool::<128, 128, 7>::new(1).await);
        let hasher = Arc::new(Mutex::new(pool.get_hasher().await.unwrap()));
        let mut hasher = hasher.lock().await;
        hasher.set_header_u64(data.len().try_into().unwrap());
        hasher.write(&data).await.unwrap();
        let res_hash = hasher.hash().await.unwrap();

        assert_eq!(
            res_hash,
            b256!("ca6357a08e317d15ec560fef34e4c45f8f19f01c372aa70f1da72bfa7f1a4338")
        );
    }

    #[tokio::test]
    async fn test_concurrent_fullsize() {
        let data: Vec<u8> = (0..4096).map(|_| rand::random::<u8>()).collect();

        let pool = Arc::new(Pool::<128, 128, 7>::new(1).await);
        let hasher = Arc::new(Mutex::new(pool.get_hasher().await.unwrap()));
        test_hasher_correctness(hasher, &data).await;
    }

    // Test correctness by comparing against the reference implementation
    async fn test_hasher_correctness<const N: usize, const W: usize, const DEPTH: usize>(
        hasher: Arc<Mutex<Hasher<N, W, DEPTH>>>,
        data: &[u8],
    ) where
        [(); W * SEGMENT_SIZE]:,
        [(); DEPTH + 1]:,
    {
        let exp_hash = ref_hash::<N>(data);
        let res_hash = sync_hash(hasher, data).await;

        assert_eq!(
            exp_hash, res_hash,
            "Hash mismatch: expected {:?} got {:?}",
            exp_hash, res_hash
        );
    }

    macro_rules! generate_tests {
        ($($segment_count:expr),*) => {
            // Nested macro for common setup
            macro_rules! common_setup {
                ($count:expr) => {
                    const N: usize = $count;
                    const PARAMS: (usize, usize) = size_to_params(N);
                    const W: usize = PARAMS.0;
                    const DEPTH: usize = PARAMS.1;
                };
            }
            $(
                paste! {
                    #[tokio::test]
                    async fn [<test_hasher_empty_data_ $segment_count>]() {
                        common_setup!($segment_count);

                        let pool = Arc::new(Pool::new(POOL_SIZE).await);
                        let hasher: Arc<Mutex<Hasher<N, W, DEPTH>>> = Arc::new(Mutex::new(pool.get_hasher().await.unwrap()));

                        test_hasher_correctness(hasher, &[]).await;
                    }

                    #[tokio::test]
                    async fn [<test_sync_hasher_correctness_ $segment_count>]() {
                        common_setup!($segment_count);

                        let pool = Arc::new(Pool::new(POOL_SIZE).await);
                        let test_data: Vec<u8> = (0..4096).map(|_| rand::random::<u8>()).collect();

                        let mut increment = 1;
                        for start in (0..test_data.len()).step_by(increment) {
                            let hasher: Arc<Mutex<Hasher<N, W, DEPTH>>> = Arc::new(Mutex::new(pool.get_hasher().await.unwrap()));
                            test_hasher_correctness(hasher, &test_data[..start]).await;
                            increment = (increment % 5) + 1;
                        }
                    }

                    #[tokio::test]
                    async fn [<test_hasher_reuse_ $segment_count>]() {
                        common_setup!($segment_count);

                        let pool = Arc::new(Pool::<N, W, DEPTH>::new(POOL_SIZE).await);
                        let hasher = Arc::new(Mutex::new(pool.get_hasher().await.unwrap()));

                        for _ in 0..100 {
                            let test_data: Vec<u8> = (0..4096).map(|_| rand::random::<u8>()).collect();
                            let test_length = rand::random::<usize>() % (W * SEGMENT_SIZE);
                            test_hasher_correctness(hasher.clone(), &test_data[..test_length]).await;
                        }
                    }
                }
           )*
        };
    }

    generate_tests!(1, 2, 3, 4, 5, 8, 9, 15, 16, 17, 32, 37, 42, 53, 63, 64, 65, 111, 127, 128);
}

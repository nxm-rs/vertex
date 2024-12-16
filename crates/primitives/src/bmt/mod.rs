use crate::bmt::tree::{Node, Tree};
use alloy_primitives::keccak256;
use anyhow::{anyhow, Result};
use std::sync::Arc;
use std::{cell::UnsafeCell, ptr::addr_of_mut};
use thiserror::Error;
use tokio::sync::{mpsc, Mutex};
use tokio::task::spawn_blocking;

use crate::{HASH_SIZE, SEGMENT_SIZE, SPAN_SIZE};

pub(crate) type Span = [u8; SPAN_SIZE];
pub(crate) type Segment = [u8; SEGMENT_SIZE];
pub(crate) type SegmentPair = [u8; SEGMENT_PAIR_SIZE];

pub mod chunk;
// pub mod file;
pub mod pool;
pub mod reference;
pub mod span;
pub mod tree;
use pool::{Pool, PoolConfig, DEPTH};

const SEGMENT_PAIR_SIZE: usize = 2 * SEGMENT_SIZE;

const ZERO_SPAN: Span = [0u8; SPAN_SIZE];
const ZERO_SEGMENT_PAIR: SegmentPair = [0u8; SEGMENT_PAIR_SIZE];
const ZERO_SEGMENT: Segment = [0u8; SEGMENT_SIZE];

const DEFAULT_MAX_PAYLOAD_SIZE: usize = 4096;
const DEFAULT_MIN_PAYLOAD_SIZE: usize = 1;

#[derive(Debug)]
pub struct Hasher {
    config: Arc<PoolConfig>,
    bmt: UnsafeCell<Tree>,
    size: usize,
    pos: usize,
    span: Span,
    // Channels
    result_tx: Option<mpsc::Sender<[u8; 32]>>,
    result_rx: Option<mpsc::Receiver<[u8; 32]>>,
    pool_tx: Option<mpsc::Sender<UnsafeCell<Tree>>>,
}

#[derive(Default)]
pub struct HasherBuilder {
    config: Option<Arc<PoolConfig>>,
    bmt: Option<UnsafeCell<Tree>>,
    pool_tx: Option<mpsc::Sender<UnsafeCell<Tree>>>,
}

impl HasherBuilder {
    /// Create a default builder whereby all options are set to `None`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Populate the builder with configuration from the respective pool..
    pub async fn with_pool(mut self, pool: Arc<Mutex<Pool>>) -> Self {
        let mut pool = pool.lock().await;

        self.config = Some(pool.config.clone());
        self.bmt = Some(pool.get().await);
        self.pool_tx = Some(pool.sender.clone());

        self
    }

    /// Use the respective [`PoolConfig`], which is essentially just for the zero hash sister
    /// lookup table.
    pub fn with_config(mut self, config: Arc<PoolConfig>) -> Self {
        self.config = Some(config);
        self
    }

    /// Use the respective [`Tree`] for building the BMT. This allows for resource reuse and
    /// prevents repetitive allocations.
    pub fn with_bmt(mut self, bmt: UnsafeCell<Tree>) -> Self {
        self.bmt = Some(bmt);
        self
    }

    /// When the [`Hasher`] drops, it will return the BMT resource back to the pool using this
    /// channel.
    pub fn with_pool_tx(mut self, pool_tx: mpsc::Sender<UnsafeCell<Tree>>) -> Self {
        self.pool_tx = Some(pool_tx);
        self
    }

    /// Given the state of the builder, construct a [`Hasher`].
    pub fn build(self) -> Result<Hasher, HashError> {
        let config = self.config.unwrap_or(Arc::new(PoolConfig::default()));
        let bmt = self.bmt.unwrap_or(Tree::new().into());
        let (result_tx, result_rx) = mpsc::channel::<[u8; 32]>(1);

        Ok(Hasher {
            config,
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
    pub async fn hash(&mut self) -> Result<[u8; 32]> {
        if self.size == 0 {
            return Ok(self.root_hash(&self.config.zero_hashes[DEPTH].clone()));
        }

        // Fill the remaining buffer with zeroes
        unsafe {
            let bmt = &mut *self.bmt.get();
            let buffer = &mut *bmt.buffer.get();
            buffer[self.size..].fill(0);
        }

        // write the last section with final flag set to true
        self.process_segment_pair(self.pos, true).await;

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
        let max_val = DEFAULT_MAX_PAYLOAD_SIZE - self.size;

        if len > max_val {
            len = max_val;
        }

        // Copy data into the internal buffer
        unsafe {
            let bmt = &mut *self.bmt.get();
            let buffer = &mut *bmt.buffer.get();
            buffer[self.size..self.size + len].copy_from_slice(&data[..len]);
        }

        // Calculate segment properties
        let from = self.size / SEGMENT_PAIR_SIZE;
        let mut to = (self.size + len) / SEGMENT_PAIR_SIZE;
        self.size += len;

        if len == max_val {
            to -= 1;
        }
        self.pos = to;

        let self_handle = addr_of_mut!(*self) as usize;

        for i in from..to {
            let task_handle = self_handle;

            spawn_blocking(move || unsafe {
                let hasher = &mut *(task_handle as *mut Hasher);
                tokio::runtime::Handle::current().block_on(async {
                    hasher.process_segment_pair(i, false).await;
                })
            });
        }

        Ok(len)
    }

    fn reset(&mut self) {
        (self.pos, self.size, self.span) = (0, 0, ZERO_SPAN);

        let (tx, rx) = mpsc::channel::<[u8; 32]>(1);
        self.result_tx = Some(tx);
        self.result_rx = Some(rx);
    }

    // Writes the hash of the i-th segment pair into level 1 node of the BMT tree.
    async fn process_segment_pair(&mut self, i: usize, is_final: bool) {
        let offset = i * SEGMENT_PAIR_SIZE;
        let level = 1;

        // Select the leaf node for the segment pair
        let (n, is_left, segment_pair_hash) = unsafe {
            let tree = &mut *self.bmt.get();
            let n = &*tree.leaves[i];

            // Correctly dereference buffer before indexing
            let buffer = &*tree.buffer.get();
            let segment_pair_hash = keccak256(&buffer[offset..offset + SEGMENT_PAIR_SIZE]);

            (n.parent.clone(), n.is_left, segment_pair_hash)
        };
        // write hash into parent node
        match is_final {
            true => {
                self.write_final_node(level, n, is_left, Some(*segment_pair_hash))
                    .await
            }
            false => self.write_node(n, is_left, *segment_pair_hash).await,
        }
    }

    /// Pushes the data to the node.
    /// If it is the first of 2 sisters written, the routine terminates.
    /// If it is the second, it calcualtes the hash and writes it to the
    /// parent node recursively.
    async fn write_node(
        &self,
        mut node: Option<Arc<Node>>,
        mut is_left: bool,
        mut segment: [u8; HASH_SIZE],
    ) {
        while let Some(node_ref) = node {
            unsafe {
                let node_mut = &mut *(Arc::as_ptr(&node_ref) as *mut Node);
                node_mut.set(is_left, segment);

                // If the first arriving thread, terminate
                if node_mut.toggle() {
                    return;
                }

                // Recompute the hash and traverse upwards
                segment = node_mut.hash_segment();
                is_left = node_mut.is_left;
                node = node_mut.parent.clone();
            }
        }

        // Reached the root of the BMT - send it!
        self.send_segment(segment).await;
    }

    /// Follow the path starting from the final data segment to the BMT root via parents.
    /// For unbalanced trees it fills in the missing right sister nodes using the pool's lookup
    /// table for BMT subtree root hashes for all-zero sections.
    /// Otherwise behaves like `write_node`.
    async fn write_final_node(
        &self,
        mut level: usize,
        mut node: Option<Arc<Node>>,
        mut is_left: bool,
        mut segment: Option<[u8; HASH_SIZE]>,
    ) {
        while let Some(node_ref) = node {
            let mut no_hash = false;

            unsafe {
                let node_mut = &mut *(Arc::as_ptr(&node_ref) as *mut Node);

                match is_left {
                    // Coming from left sister branch
                    // When the final segment's path is going via left child node we include an
                    // all-zero subtree hash for the right level and toggle the node.
                    true => {
                        node_mut.set(false, self.config.as_ref().zero_hashes[level]);
                        if let Some(seg) = segment {
                            // If a left final node carries a hash, it must be the first (and only
                            // thread), so the toggle is already in passive state. No need to call
                            // yet thread needs to carry on pushing hash to parent.
                            node_mut.set(true, seg);
                            no_hash = false;
                        } else {
                            // If the first thread then propagate None and calcualte no hash
                            no_hash = node_mut.toggle();
                        }
                    }
                    false => {
                        if let Some(seg) = segment {
                            // If hash was pushed from right child node, write right segment change
                            // state
                            node_mut.set(false, seg);
                            // If toggle is true, we arrived first so no hashing just push None to
                            // parent.
                            no_hash = node_mut.toggle();
                        } else {
                            // If sister is None, then thread arrived first at previous node and
                            // here there will be two so no need to do anything and keep sister =
                            // None for parent.
                            no_hash = true;
                        }
                    }
                }

                segment = if no_hash {
                    None
                } else {
                    Some(node_mut.hash_segment())
                };

                is_left = node_mut.is_left;
                node = node_mut.parent.clone();
            }
            level += 1;
        }

        if let Some(seg) = segment {
            self.send_segment(seg).await;
        }
    }

    async fn send_segment(&self, segment: [u8; HASH_SIZE]) {
        if let Some(tx) = &self.result_tx {
            let tx = tx.clone();
            if let Err(_e) = tx.send(segment).await {
                todo!("Add error tracing here");
            }
        }
    }

    fn root_hash(&self, last: &[u8]) -> [u8; 32] {
        let mut input = [0u8; SPAN_SIZE + HASH_SIZE];

        input[..SPAN_SIZE].copy_from_slice(&self.span[..]);
        input[SPAN_SIZE..(SPAN_SIZE + HASH_SIZE)].copy_from_slice(last);

        *keccak256(input)
    }

    /// Set the header bytes of BMT hash by copying the first 8 bytes of the argument
    fn set_header_bytes(&mut self, header: &[u8]) -> Result<(), HashError> {
        let length = header.len();
        match length == SPAN_SIZE {
            true => {
                self.span.copy_from_slice(&header[0..SPAN_SIZE]);
                Ok(())
            }
            false => Err(HashError::InvalidLength(length)),
        }
    }

    pub fn set_header_u64(&mut self, header: u64) {
        self.span = length_to_span(header);
    }
}

//impl Drop for Hasher {
//    fn drop(&mut self) {
//        if let Some(tx) = &self.pool_tx {
//            let value = unsafe { *self.bmt.get() };
//            tokio::spawn(async move {
//                if let Err(e) = tx.send(value.into()).await {
//                    eprintln!("Error sending data through the async channel: {:?}", e);
//                }
//            });
//        }
//    }
//}

/// Creates a binary data span size representation - required for calcualting the BMT hash
fn length_to_span(length: u64) -> Span {
    length.to_le_bytes()
}

/// Returns length from span
fn length_from_span(span: Span) -> u64 {
    u64::from_le_bytes(span)
}

#[cfg(test)]
mod tests {
    use pool::{Pool, PooledHasher};
    use rand::Rng;
    use reference::RefHasher;

    use super::*;

    const POOL_SIZE: usize = 16;
    const SEGMENT_COUNT: usize = 128;

    fn ref_hash(count: usize, data: &[u8]) -> [u8; 32] {
        let ref_bmt = RefHasher::new(count);
        let ref_no_metahash = ref_bmt.hash(data);

        *keccak256(
            [
                length_to_span(data.len().try_into().unwrap()).as_slice(),
                ref_no_metahash.as_slice(),
            ]
            .concat(),
        )
    }

    async fn sync_hash(hasher: Arc<Mutex<Hasher>>, data: &[u8]) -> [u8; 32] {
        let mut hasher = hasher.lock().await;
        hasher.reset();

        hasher.set_header_u64(data.len().try_into().unwrap());
        let n = hasher.write(data).await.unwrap();
        println!("Wrote {} bytes to the hasher", n);

        hasher.hash().await.unwrap()
    }

    #[tokio::test]
    async fn concurrent_hash() {
        let pool = Arc::new(Mutex::new(Pool::new(POOL_SIZE).await));
        let mut hasher = Arc::new(Mutex::new(pool.get_hasher().await.unwrap()));

        let mut data = vec![0u8; 64];
        // rand::thread_rng().fill(&mut data[..]);

        let concurrent = sync_hash(hasher, &data).await;

        println!("hash produced was: {:?}", concurrent);

        let rhash = ref_hash(128, &data);

        assert_eq!(concurrent, rhash);
    }
}

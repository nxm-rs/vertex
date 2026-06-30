//! Portable postage usage for the browser client: cross-device snapshot
//! open/flush over the routing provider/sender.

use std::sync::Arc;

use alloy_primitives::Address;
use alloy_signer_local::PrivateKeySigner;
use nectar_postage::Batch;
use nectar_postage_usage::{
    PublishedSequence, RootInfo, SealedChunk, Snapshot, SnapshotSink, SnapshotSource, SwarmAddress,
    seal_plan, usage_chunk_address,
};
use nectar_primitives::AnyChunk;
use nectar_primitives::bytes::Bytes;
use vertex_swarm_api::{StampedChunk, SwarmChunkProvider, SwarmChunkSender, SwarmError};
use wasm_bindgen::JsValue;

/// `Send + Sync` transport error for the browser usage adapters (never absence).
#[derive(Debug)]
pub struct UsageAdapterError(String);

impl core::fmt::Display for UsageAdapterError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "swarm usage transport error: {}", self.0)
    }
}

impl core::error::Error for UsageAdapterError {}

impl From<SwarmError> for UsageAdapterError {
    fn from(e: SwarmError) -> Self {
        Self(e.to_string())
    }
}

/// A [`SnapshotSource`] over the browser routing provider; not-found maps to
/// `Ok(None)`, every other failure to `Err` (floor safety).
#[derive(Clone)]
pub struct BrowserUsageSource {
    provider: Arc<dyn SwarmChunkProvider>,
}

impl BrowserUsageSource {
    /// Build a source over the client's shared routing provider.
    pub fn new(provider: Arc<dyn SwarmChunkProvider>) -> Self {
        Self { provider }
    }
}

impl SnapshotSource for BrowserUsageSource {
    type Error = UsageAdapterError;

    async fn fetch(&self, address: &SwarmAddress) -> Result<Option<Bytes>, Self::Error> {
        match self.provider.retrieve_chunk(address).await {
            // A retrieved chunk is already address-validated (owner + signature
            // for a single-owner chunk). Its data payload is the snapshot payload
            // the codec parses; hand back a clone of it.
            Ok(result) => Ok(Some(result.chunk.data().clone())),
            // Retrieval exhausted the reachable peers: best-effort snapshot, so
            // treat the chunk as absent rather than failing the open.
            Err(SwarmError::RetrievalExhausted { .. }) => Ok(None),
            // Any other error is a read that could not be completed.
            Err(e) => Err(UsageAdapterError::from(e)),
        }
    }
}

/// A [`SnapshotSink`] over the browser routing sender.
#[derive(Clone)]
pub struct BrowserUsageSink {
    sender: Arc<dyn SwarmChunkSender>,
}

impl BrowserUsageSink {
    /// Build a sink over the client's shared routing sender.
    pub fn new(sender: Arc<dyn SwarmChunkSender>) -> Self {
        Self { sender }
    }
}

impl SnapshotSink for BrowserUsageSink {
    type Error = UsageAdapterError;

    async fn push(&self, sealed: &SealedChunk) -> Result<(), Self::Error> {
        // The seal produced both the signed single-owner chunk and its stamp.
        let chunk: AnyChunk = AnyChunk::from(sealed.chunk.clone());
        let stamped = StampedChunk::new(chunk, sealed.stamp.clone());
        self.sender.send_chunk(stamped).await?;
        Ok(())
    }
}

/// Recover the published usage snapshot for `batch`, or start fresh on confirmed
/// absence (a transport failure aborts rather than downgrading).
pub async fn open_snapshot(
    source: &BrowserUsageSource,
    batch: &Batch,
) -> Result<Snapshot, JsValue> {
    let batch_id = batch.id();
    let owner = batch.owner();
    let root_addr = usage_chunk_address(&batch_id, &owner, 0);

    match source.fetch(&root_addr).await.map_err(usage_err)? {
        Some(root_bytes) => {
            let root = RootInfo::parse(&root_bytes).map_err(usage_err)?;
            let mut leaves: Vec<Bytes> = Vec::with_capacity(root.leaf_count() as usize);
            for leaf in 0..root.leaf_count() {
                let index = leaf + 1;
                let leaf_addr = usage_chunk_address(&batch_id, &owner, index);
                match source.fetch(&leaf_addr).await.map_err(usage_err)? {
                    Some(bytes) => leaves.push(bytes),
                    None => {
                        return Err(JsValue::from_str(&format!(
                            "published usage root commits to leaf {index} but the network \
                             reports it absent (snapshot corruption)"
                        )));
                    }
                }
            }
            root.assemble(&leaves).map_err(usage_err)
        }
        // The network confirms no published root: a genuinely fresh batch.
        None => Snapshot::from_batch(batch).map_err(usage_err),
    }
}

/// Persist the snapshot back to Swarm: re-read the live floor, revalidate, plan,
/// seal with a strictly-increasing timestamp, and push every sealed chunk.
pub async fn flush_snapshot(
    snapshot: &mut Snapshot,
    owner: &Address,
    signer: &PrivateKeySigner,
    source: &BrowserUsageSource,
    sink: &BrowserUsageSink,
) -> Result<(), JsValue> {
    let batch_id = snapshot.table().batch_id();
    let root_addr = usage_chunk_address(&batch_id, owner, 0);

    // Re-read the live root to derive the published floor: a transport failure
    // aborts rather than persisting against a floor it could not read.
    let floor = match source.fetch(&root_addr).await.map_err(usage_err)? {
        Some(root_bytes) => {
            PublishedSequence::from(&RootInfo::parse(&root_bytes).map_err(usage_err)?)
        }
        None => PublishedSequence::NONE,
    };

    let plan = snapshot
        .revalidate(floor)
        .map_err(usage_err)?
        .plan_persist(owner)
        .map_err(usage_err)?;

    // The seal timestamp must strictly increase across flushes so the reserve
    // overwrites each metadata chunk in place. Take the wall clock (seconds),
    // lifted past the previous seal so a coarse clock never trips the in-process
    // guard.
    let now = (vertex_util_runtime::time::now_unix_nanos().max(0) as u64) / 1_000_000_000;
    let timestamp = snapshot
        .last_seal_timestamp()
        .map_or(now, |previous| now.max(previous + 1));

    let sealed = seal_plan(snapshot, &plan, timestamp, signer).map_err(usage_err)?;
    for chunk in &sealed {
        sink.push(chunk).await.map_err(usage_err)?;
    }

    Ok(())
}

/// Render any usage-ceremony error as a `JsValue` for the wasm surface.
fn usage_err(e: impl core::fmt::Display) -> JsValue {
    JsValue::from_str(&format!("usage: {e}"))
}

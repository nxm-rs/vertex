//! Browser download + manifest-walk flow.
//!
//! Manifests are read against the in-memory cache with a network prefetch-retry
//! loop. File reassembly enumerates the chunk tree, prefetches it breadth-first
//! with real concurrency (the joiner's own DFS would serialise to ~1 in flight),
//! then assembles from the warm cache.

use std::collections::HashSet;
use std::sync::Arc;

use futures::StreamExt;
use nectar_mantaray::error::MantarayError;
use nectar_mantaray::{Entry, PlainManifest};
use nectar_primitives::store::ChunkStoreError;
use nectar_primitives::{AnyChunk, ChunkAddress, DEFAULT_BODY_SIZE, Joiner};
use vertex_swarm_api::SwarmChunkProvider;
use wasm_bindgen::JsValue;

use super::cache::MemoryCache;
use super::net_get::NetworkChunkGet;

/// Bytes of a single child reference in a plain-mode intermediate node body.
const REF_SIZE: usize = 32;

/// Maximum prefetch round trips before giving up on a manifest op.
const MAX_PREFETCH_ITERS: usize = 4096;

/// Chunk retrievals kept in flight while prefetching a file's chunk tree.
const DOWNLOAD_CONCURRENCY: usize = 32;

/// Download the file at `root`, resolving it as a single-file manifest if it is one.
pub async fn download_reference(
    root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
) -> Result<Vec<u8>, JsValue> {
    match probe_manifest_entries(root, provider.clone(), cache).await? {
        // `root` is a manifest: pick the file entry to return.
        Some(entries) => {
            let file_root = pick_manifest_file(&entries)?;
            download_file(file_root, provider, cache).await
        }
        // `root` is a plain file content chunk: join it directly.
        None => download_file(root, provider, cache).await,
    }
}

/// Probe whether `root` is a manifest, returning its entries or `None` if not.
async fn probe_manifest_entries(
    root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
) -> Result<Option<Vec<Entry>>, JsValue> {
    for _ in 0..MAX_PREFETCH_ITERS {
        let mut manifest: PlainManifest<MemoryCache> = PlainManifest::open(root, cache.clone());
        match manifest.entries().await {
            Ok(entries) if !entries.is_empty() => return Ok(Some(entries)),
            // Parsed as a manifest but empty: treat as "not a usable manifest"
            // and fall through to a raw file join.
            Ok(_) => return Ok(None),
            Err(e) => match missing_address(&e) {
                // A missing chunk: fetch it and retry the probe.
                Some(missing) => {
                    let result = provider
                        .retrieve_chunk(&missing)
                        .await
                        .map_err(|e| JsValue::from_str(&format!("retrieve {missing}: {e}")))?;
                    cache.insert(result.chunk);
                }
                // Any other parse error means the root chunk is a plain file
                // content chunk, not a mantaray node: not a manifest.
                None => return Ok(None),
            },
        }
    }
    Err(JsValue::from_str(
        "manifest probe exceeded prefetch budget; chunk likely unavailable",
    ))
}

/// Pick the file root from a manifest's `entries`, preferring `index.html` or `/`.
fn pick_manifest_file(entries: &[Entry]) -> Result<ChunkAddress, JsValue> {
    let valued: Vec<&Entry> = entries.iter().filter(|e| e.address().is_some()).collect();

    match valued.as_slice() {
        [] => Err(JsValue::from_str(
            "reference is a manifest with no value-bearing entries",
        )),
        [only] => Ok(*only.address().expect("filtered to value-bearing")),
        many => {
            if let Some(preferred) = many
                .iter()
                .find(|e| matches!(e.path_str(), Some("index.html") | Some("/")))
            {
                return Ok(*preferred.address().expect("filtered to value-bearing"));
            }
            let paths: Vec<&str> = many
                .iter()
                .map(|e| e.path_str().unwrap_or("<non-utf8>"))
                .collect();
            Err(JsValue::from_str(&format!(
                "reference is a manifest with {} files; download a specific path via walk(). entries: {}",
                many.len(),
                paths.join(", ")
            )))
        }
    }
}

/// Reassemble the file at `file_root`: prefetch its chunk tree, then join from cache.
pub async fn download_file(
    file_root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
) -> Result<Vec<u8>, JsValue> {
    prefetch_tree(file_root, provider.clone(), cache).await?;

    let getter = NetworkChunkGet::new(provider, cache.snapshot_map());
    join_to_bytes(file_root, getter).await
}

/// List the manifest at `root` as `(path, address_hex)` pairs.
pub async fn ls_manifest(
    root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
) -> Result<Vec<(String, String)>, JsValue> {
    let entries = prefetch_then(provider, cache, |c| async move {
        let mut manifest: PlainManifest<MemoryCache> = PlainManifest::open(root, c);
        manifest.entries().await
    })
    .await?;

    Ok(entries
        .iter()
        .map(|e| {
            let path = e.path_str().unwrap_or("<non-utf8>").to_string();
            let addr = e
                .address()
                .map(|a| a.to_string())
                .unwrap_or_else(|| "<none>".to_string());
            (path, addr)
        })
        .collect())
}

/// Walk `path` in the manifest at `root`, returning the referenced file's bytes.
pub async fn walk(
    root: ChunkAddress,
    path: &str,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
) -> Result<Vec<u8>, JsValue> {
    let path_owned = path.to_string();
    let entry: Entry = prefetch_then(provider.clone(), cache, |c| {
        let path = path_owned.clone();
        async move {
            let mut manifest: PlainManifest<MemoryCache> = PlainManifest::open(root, c);
            manifest.lookup(&path).await
        }
    })
    .await?;

    let file_root = entry
        .address()
        .copied()
        .ok_or_else(|| JsValue::from_str(&format!("manifest entry '{path}' has no reference")))?;

    download_file(file_root, provider, cache).await
}

/// Prefetch the chunk tree at `root` into `cache`, breadth-first and concurrent.
async fn prefetch_tree(
    root: ChunkAddress,
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
) -> Result<(), JsValue> {
    // Addresses whose chunk we have already fetched (or queued to fetch) this
    // pass: dedups shared subtrees and guards against a malformed cycle.
    let mut seen: HashSet<ChunkAddress> = HashSet::new();
    // The current level to fetch concurrently; starts with just the root.
    let mut level: Vec<ChunkAddress> = vec![root];
    seen.insert(root);

    while !level.is_empty() {
        // Fetch this whole level concurrently, skipping chunks already cached.
        let fetched: Vec<Result<AnyChunk, JsValue>> = futures::stream::iter(level.into_iter())
            .map(|addr| {
                let provider = Arc::clone(&provider);
                let cached = cache.fetch(&addr);
                async move {
                    match cached {
                        Some(chunk) => Ok(chunk),
                        None => provider
                            .retrieve_chunk(&addr)
                            .await
                            .map(|r| r.chunk)
                            .map_err(|e| JsValue::from_str(&format!("retrieve {addr}: {e}"))),
                    }
                }
            })
            .buffer_unordered(DOWNLOAD_CONCURRENCY)
            .collect()
            .await;

        // Insert the fetched chunks and gather the next level (children of the
        // intermediate nodes). A retrieval error here fails the download.
        let mut next: Vec<ChunkAddress> = Vec::new();
        for result in fetched {
            let chunk = result?;
            // Intermediate node ⇒ its body is packed child references; a leaf's
            // span fits one chunk body and ends the branch.
            if chunk.span() > DEFAULT_BODY_SIZE as u64 {
                for child in parse_child_refs(chunk.data())? {
                    if seen.insert(child) {
                        next.push(child);
                    }
                }
            }
            cache.insert(chunk);
        }
        level = next;
    }

    Ok(())
}

/// Parse an intermediate node's body as packed 32-byte child chunk addresses.
fn parse_child_refs(body: &[u8]) -> Result<Vec<ChunkAddress>, JsValue> {
    if body.len() % REF_SIZE != 0 {
        return Err(JsValue::from_str(&format!(
            "malformed intermediate node: body length {} is not a multiple of {REF_SIZE}",
            body.len()
        )));
    }
    Ok(body
        .chunks_exact(REF_SIZE)
        .map(|r| {
            let mut arr = [0u8; REF_SIZE];
            arr.copy_from_slice(r);
            ChunkAddress::new(arr)
        })
        .collect())
}

/// Reassemble the file at `root` from the warm cache-backed `getter`.
async fn join_to_bytes(root: ChunkAddress, getter: NetworkChunkGet) -> Result<Vec<u8>, JsValue> {
    let joiner = Joiner::<NetworkChunkGet, DEFAULT_BODY_SIZE>::new(getter, root)
        .await
        .map_err(|e| JsValue::from_str(&format!("joiner open: {e}")))?
        .with_concurrency(DOWNLOAD_CONCURRENCY);

    if joiner.size() == 0 {
        return Ok(Vec::new());
    }

    joiner
        .read_all()
        .await
        .map_err(|e| JsValue::from_str(&format!("joiner read: {e}")))
}

/// Run a mantaray op against the cache, fetching missing chunks and retrying.
async fn prefetch_then<T, F, Fut>(
    provider: Arc<dyn SwarmChunkProvider>,
    cache: &MemoryCache,
    mut op: F,
) -> Result<T, JsValue>
where
    F: FnMut(MemoryCache) -> Fut,
    Fut: std::future::Future<Output = Result<T, MantarayError>>,
{
    for _ in 0..MAX_PREFETCH_ITERS {
        match op(cache.clone()).await {
            Ok(value) => return Ok(value),
            Err(e) => {
                let missing = missing_address(&e).ok_or_else(|| {
                    JsValue::from_str(&format!("manifest op failed (not a missing chunk): {e}"))
                })?;
                // Fetch the missing chunk over the network and cache it, then
                // retry the whole operation (mantaray will get further this time).
                let result = provider
                    .retrieve_chunk(&missing)
                    .await
                    .map_err(|e| JsValue::from_str(&format!("retrieve {missing}: {e}")))?;
                cache.insert(result.chunk);
            }
        }
    }
    Err(JsValue::from_str(
        "manifest walk exceeded prefetch budget; chunk likely unavailable",
    ))
}

/// If `err` is a store-get miss, extract the missing chunk address.
fn missing_address(err: &MantarayError) -> Option<ChunkAddress> {
    let MantarayError::StoreGet { source } = err else {
        return None;
    };
    let store_err = source.downcast_ref::<ChunkStoreError>()?;
    let ChunkStoreError::NotFound(address) = store_err else {
        return None;
    };
    Some(*address)
}

//! Inbound and outbound protocol handlers for hive peer exchange.

use std::sync::Arc;

use parking_lot::Mutex;
use std::time::Instant;

use futures::future::BoxFuture;
use hashlink::LruCache;
use metrics::{counter, histogram};
use tracing::{debug, warn};
use vertex_net_codec::FramedProto;
use vertex_net_utils::extract_peer_id;
use vertex_swarm_api::{SwarmIdentity, SwarmSpec};
use vertex_swarm_net_headers::{
    HeaderedInbound, HeaderedOutbound, HeaderedStream, Outbound, ProtocolStreamError,
};
use vertex_swarm_peer::{SwarmAddress, SwarmPeer};
use vertex_tasks::TaskExecutor;

use crate::PROTOCOL_NAME;
use crate::bzz::BzzAddress;
use crate::codec::encode_peers;
use crate::verifier::{DefaultHiveVerifier, GossipSource, HiveRejection, HiveVerifier};

/// 32 KiB frame limit (fits ~100 peers at typical size).
const MAX_MESSAGE_SIZE: usize = 32 * 1024;

/// Maximum validated peers to cache (bounds memory).
const PEER_CACHE_CAPACITY: usize = 256;

type Framed = FramedProto<MAX_MESSAGE_SIZE>;

/// Shared cache of recently validated peers, keyed by overlay address.
pub(crate) type PeerCache = Arc<Mutex<LruCache<SwarmAddress, SwarmPeer>>>;

/// Create a new peer validation cache.
pub(crate) fn new_peer_cache() -> PeerCache {
    Arc::new(Mutex::new(LruCache::new(PEER_CACHE_CAPACITY)))
}

/// Result of inbound peer validation.
#[derive(Debug)]
pub struct ValidatedPeers {
    pub(crate) peers: Vec<SwarmPeer>,
}

/// Inbound handler that receives and validates peers.
pub struct HiveInner<I: SwarmIdentity> {
    identity: Arc<I>,
    cache: PeerCache,
}

impl<I: SwarmIdentity> std::fmt::Debug for HiveInner<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HiveInner").finish_non_exhaustive()
    }
}

impl<I: SwarmIdentity> HiveInner<I> {
    pub(crate) fn new(identity: Arc<I>, cache: PeerCache) -> Self {
        Self { identity, cache }
    }
}

impl<I: SwarmIdentity> HeaderedInbound for HiveInner<I> {
    type Output = ValidatedPeers;
    type Error = ProtocolStreamError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn read(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        let network_id = self.identity.spec().network_id();
        let local_overlay = self.identity.overlay_address();
        let cache = self.cache;
        Box::pin(async move {
            use vertex_swarm_net_proto::hive::Peers;

            debug!(network_id, "Hive: reading peers");
            let (proto, _) =
                Framed::recv::<Peers, ProtocolStreamError, _>(stream.into_inner()).await?;

            let raw_peers = proto.peers;

            // Offload CPU-bound ECDSA validation to blocking thread pool. The
            // verifier owns both signature recovery and timestamp-skew
            // validation; rejections are mapped to stable metric labels.
            let verifier: Arc<dyn HiveVerifier> =
                Arc::new(DefaultHiveVerifier::new(network_id, local_overlay));
            let (peers, valid_count, invalid_count) =
                validate_batch_blocking(raw_peers, verifier, cache).await;

            // Hive-specific peer metrics
            counter!("hive_peers_received_total", "outcome" => "valid")
                .increment(valid_count as u64);
            counter!("hive_peers_received_total", "outcome" => "invalid")
                .increment(invalid_count as u64);
            histogram!("hive_peers_per_exchange", "direction" => "inbound")
                .record(valid_count as f64);

            Ok(ValidatedPeers { peers })
        })
    }
}

/// Validate a batch of proto peers, offloading to a blocking thread if the executor is available.
async fn validate_batch_blocking(
    raw_peers: Vec<vertex_swarm_net_proto::hive::Peer>,
    verifier: Arc<dyn HiveVerifier>,
    cache: PeerCache,
) -> (Vec<SwarmPeer>, usize, usize) {
    let Ok(executor) = TaskExecutor::try_current() else {
        return validate_batch(raw_peers, verifier.as_ref(), &cache);
    };

    let (tx, rx) = tokio::sync::oneshot::channel();
    executor.spawn_blocking("hive_peer_validation", async move {
        let result = validate_batch(raw_peers, verifier.as_ref(), &cache);
        let _ = tx.send(result);
    });

    rx.await.unwrap_or_default()
}

/// Validate a batch of proto peers (CPU-bound).
///
/// Returns (valid_peers, valid_count, invalid_count).
pub(crate) fn validate_batch(
    raw_peers: Vec<vertex_swarm_net_proto::hive::Peer>,
    verifier: &dyn HiveVerifier,
    cache: &Mutex<LruCache<SwarmAddress, SwarmPeer>>,
) -> (Vec<SwarmPeer>, usize, usize) {
    let validation_start = Instant::now();
    let mut valid_count = 0usize;
    let mut invalid_count = 0usize;

    let peers: Vec<SwarmPeer> = raw_peers
        .into_iter()
        .filter_map(|p| match validate_proto_peer(p, verifier, cache) {
            Ok(peer) => {
                valid_count += 1;
                Some(peer)
            }
            Err(reason) => {
                record_rejection(reason);
                invalid_count += 1;
                None
            }
        })
        .collect();

    histogram!("hive_validation_duration_seconds", "direction" => "inbound")
        .record(validation_start.elapsed().as_secs_f64());

    (peers, valid_count, invalid_count)
}

fn record_rejection(reason: HiveRejection) {
    let label: &'static str = reason.into();
    counter!("hive_peer_validation_failures_total", "reason" => label).increment(1);
}

/// Validate and convert a single proto peer using the supplied [`HiveVerifier`].
///
/// Two-tier lookup to avoid redundant ECDSA signature recovery:
/// 1. LRU cache — validated earlier in this session (matching signature bytes)
/// 2. Full `BzzAddress` parse + verifier dispatch — cold path
pub(crate) fn validate_proto_peer(
    p: vertex_swarm_net_proto::hive::Peer,
    verifier: &dyn HiveVerifier,
    cache: &Mutex<LruCache<SwarmAddress, SwarmPeer>>,
) -> Result<SwarmPeer, HiveRejection> {
    let addr = BzzAddress::from_wire(
        p.multiaddrs,
        &p.signature,
        &p.overlay,
        &p.nonce,
        p.timestamp,
        &p.chequebook_address,
    )
    .map_err(HiveRejection::from)?;

    let overlay_for_cache = addr.overlay;

    // Tier 1: Check LRU cache by overlay+signature — verified earlier in this session.
    // On signature mismatch the cold path overwrites the entry.
    {
        let mut guard = cache.lock();
        if let Some(cached) = guard.get(&overlay_for_cache)
            && cached.signature() == &addr.signature
        {
            counter!("hive_validation_cache_total", "outcome" => "cache_hit").increment(1);
            return Ok(cached.clone());
        }
    }
    counter!("hive_validation_cache_total", "outcome" => "miss").increment(1);

    // Tier 2: Full verification via the trait. Every received peer record
    // must pass signature + timestamp-skew validation before reaching the
    // routing table (kademlia AddPeer).
    let verified = verifier.verify(&addr, GossipSource::Gossip)?;
    let peer = verified.peer;

    // Reject if any multiaddr lacks a `/p2p/` component — without one we
    // cannot dial the peer back. Bee strips/normalises these upstream.
    if !peer
        .multiaddrs()
        .iter()
        .all(|addr| extract_peer_id(addr).is_some())
    {
        warn!(
            overlay = %peer.overlay(),
            "rejecting peer: multiaddrs missing /p2p/ component"
        );
        return Err(HiveRejection::Malformed);
    }

    // Store verified peer in LRU cache.
    cache.lock().insert(overlay_for_cache, peer.clone());

    Ok(peer)
}

/// Outbound handler that sends peers to remote.
#[derive(Debug, Clone)]
pub struct HiveOutboundInner {
    proto: vertex_swarm_net_proto::hive::Peers,
    peer_count: usize,
}

impl HiveOutboundInner {
    pub(crate) fn new(peers: &[SwarmPeer]) -> Self {
        Self {
            proto: encode_peers(peers),
            peer_count: peers.len(),
        }
    }
}

impl HeaderedOutbound for HiveOutboundInner {
    type Output = ();
    type Error = ProtocolStreamError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn write(
        self,
        stream: HeaderedStream,
    ) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        let peer_count = self.peer_count;
        Box::pin(async move {
            debug!(peer_count, "Hive: sending peers");
            Framed::send::<_, ProtocolStreamError, _>(stream.into_inner(), self.proto).await?;

            // Hive-specific peer metrics
            counter!("hive_peers_sent_total").increment(peer_count as u64);
            histogram!("hive_peers_per_exchange", "direction" => "outbound")
                .record(peer_count as f64);

            Ok(())
        })
    }
}

/// Outbound protocol upgrade with header exchange.
pub(crate) type HiveOutboundProtocol = Outbound<HiveOutboundInner>;

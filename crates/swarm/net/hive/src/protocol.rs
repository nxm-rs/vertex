//! Inbound and outbound protocol handlers for hive peer exchange.

use std::sync::Arc;

use libp2p::PeerId;
use parking_lot::Mutex;
use std::time::Instant;

use alloy_primitives::{B256, Signature};
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
use vertex_swarm_peer::{SwarmAddress, SwarmPeer, Timestamp};
use vertex_swarm_primitives::{NetworkId, Nonce};
use vertex_tasks::TaskExecutor;

use crate::PROTOCOL_NAME;
use crate::codec::encode_peers;
use crate::error::ValidationFailure;
use crate::metrics::{
    DISCARD_REASON_BOOTNODE_MODE, DISCARD_REASON_RATE_LIMITED, DISCARD_REASON_VERIFIER_REJECTED,
    HIVE_PEERS_DISCARDED_TOTAL,
};
use crate::peer_handler::{HivePeerHandler, InboundPolicy};
use vertex_net_ratelimiter::KeyedRateLimiter;

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
    remote_peer_id: PeerId,
    inbound_limit: Arc<KeyedRateLimiter<PeerId>>,
    peer_handler: Arc<dyn HivePeerHandler>,
}

impl<I: SwarmIdentity> std::fmt::Debug for HiveInner<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HiveInner").finish_non_exhaustive()
    }
}

impl<I: SwarmIdentity> HiveInner<I> {
    pub(crate) fn new(
        identity: Arc<I>,
        cache: PeerCache,
        remote_peer_id: PeerId,
        inbound_limit: Arc<KeyedRateLimiter<PeerId>>,
        peer_handler: Arc<dyn HivePeerHandler>,
    ) -> Self {
        Self {
            identity,
            cache,
            remote_peer_id,
            inbound_limit,
            peer_handler,
        }
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
        let remote_peer_id = self.remote_peer_id;
        let inbound_limit = self.inbound_limit;
        let peer_handler = self.peer_handler;
        Box::pin(async move {
            use vertex_swarm_net_proto::hive::Peers;

            debug!(%network_id, "Hive: reading peers");
            let (proto, _) =
                Framed::recv::<Peers, ProtocolStreamError, _>(stream.into_inner()).await?;

            let raw_peers = proto.peers;
            let raw_count = raw_peers.len();

            // Charge the rate-limit on the raw wire count before any ECDSA
            // work, so a flood of invalid signatures cannot bypass throttling
            // by being filtered out.
            if raw_count > 0 {
                let cost = u32::try_from(raw_count).unwrap_or(u32::MAX);
                if inbound_limit.try_consume_n(remote_peer_id, cost).is_err() {
                    counter!(HIVE_PEERS_DISCARDED_TOTAL, "reason" => DISCARD_REASON_RATE_LIMITED)
                        .increment(raw_count as u64);
                    warn!(%remote_peer_id, raw_count, "hive: inbound rate limit exceeded");
                    return Ok(ValidatedPeers { peers: Vec::new() });
                }
            }

            // Bootnodes drop inbound gossip without validating it: the records
            // would never be ingested anyway, so spending the per-batch ECDSA
            // cost is pointless. The metric counts the raw wire peers, not the
            // post-validation count.
            if matches!(peer_handler.policy(), InboundPolicy::Discard) {
                counter!(HIVE_PEERS_DISCARDED_TOTAL, "reason" => DISCARD_REASON_BOOTNODE_MODE)
                    .increment(raw_count as u64);
                debug!(%remote_peer_id, raw_count, "hive: bootnode discard");
                return Ok(ValidatedPeers { peers: Vec::new() });
            }

            let (peers, valid_count, invalid_count) =
                validate_batch_blocking(raw_peers, network_id, local_overlay, cache).await;

            counter!("hive_peers_received_total", "outcome" => "valid")
                .increment(valid_count as u64);
            counter!("hive_peers_received_total", "outcome" => "invalid")
                .increment(invalid_count as u64);
            if invalid_count > 0 {
                counter!(HIVE_PEERS_DISCARDED_TOTAL, "reason" => DISCARD_REASON_VERIFIER_REJECTED)
                    .increment(invalid_count as u64);
            }
            histogram!("hive_peers_per_exchange", "direction" => "inbound")
                .record(valid_count as f64);

            Ok(ValidatedPeers { peers })
        })
    }
}

/// Validate a batch of proto peers, offloading to a blocking thread if the executor is available.
async fn validate_batch_blocking(
    raw_peers: Vec<vertex_swarm_net_proto::hive::SwarmPeer>,
    network_id: NetworkId,
    local_overlay: SwarmAddress,
    cache: PeerCache,
) -> (Vec<SwarmPeer>, usize, usize) {
    let Ok(executor) = TaskExecutor::try_current() else {
        return validate_batch(raw_peers, network_id, &local_overlay, &cache);
    };

    let (tx, rx) = tokio::sync::oneshot::channel();
    executor.spawn_blocking("hive_peer_validation", async move {
        let result = validate_batch(raw_peers, network_id, &local_overlay, &cache);
        let _ = tx.send(result);
    });

    rx.await.unwrap_or_default()
}

/// Validate a batch of proto peers (CPU-bound).
///
/// Returns (valid_peers, valid_count, invalid_count).
fn validate_batch(
    raw_peers: Vec<vertex_swarm_net_proto::hive::SwarmPeer>,
    network_id: NetworkId,
    local_overlay: &SwarmAddress,
    cache: &Mutex<LruCache<SwarmAddress, SwarmPeer>>,
) -> (Vec<SwarmPeer>, usize, usize) {
    let validation_start = Instant::now();
    let mut valid_count = 0usize;
    let mut invalid_count = 0usize;

    let peers: Vec<SwarmPeer> = raw_peers
        .into_iter()
        .filter_map(
            |p| match validate_proto_peer(p, network_id, local_overlay, cache) {
                Ok(peer) => {
                    valid_count += 1;
                    Some(peer)
                }
                Err(reason) => {
                    reason.record();
                    invalid_count += 1;
                    None
                }
            },
        )
        .collect();

    histogram!("hive_validation_duration_seconds", "direction" => "inbound")
        .record(validation_start.elapsed().as_secs_f64());

    (peers, valid_count, invalid_count)
}

/// Validate and convert a single proto peer.
///
/// Two-tier lookup to avoid redundant ECDSA signature recovery:
/// 1. LRU cache — validated earlier in this session
/// 2. Full ECDSA recovery — cold path
fn validate_proto_peer(
    p: vertex_swarm_net_proto::hive::SwarmPeer,
    network_id: NetworkId,
    local_overlay: &SwarmAddress,
    cache: &Mutex<LruCache<SwarmAddress, SwarmPeer>>,
) -> Result<SwarmPeer, ValidationFailure> {
    let overlay = B256::try_from(p.overlay.as_slice()).map_err(ValidationFailure::OverlayLength)?;

    // Reject our own overlay address to prevent self-dial
    let peer_overlay = SwarmAddress::from(overlay);
    if peer_overlay == *local_overlay {
        debug!("Hive: rejected self-overlay from peer exchange");
        return Err(ValidationFailure::SelfOverlay);
    }

    let signature = Signature::try_from(p.signature.as_slice())?;

    // Tier 1: Check LRU cache - validated earlier in this session.
    // On signature mismatch, falls through to tier 2 which re-validates and overwrites.
    {
        let mut guard = cache.lock();
        if let Some(cached) = guard.get(&peer_overlay)
            && *cached.signature() == signature
        {
            counter!("hive_validation_cache_total", "outcome" => "cache_hit").increment(1);
            return Ok(cached.clone());
        }
    }
    counter!("hive_validation_cache_total", "outcome" => "miss").increment(1);

    // Tier 2: Full ECDSA signature recovery via SwarmPeer::parse.
    let nonce_bytes: [u8; 32] = p
        .nonce
        .as_slice()
        .try_into()
        .map_err(ValidationFailure::NonceLength)?;
    let nonce = Nonce::new(nonce_bytes);
    let timestamp = Timestamp::from_seconds(p.timestamp);

    let wire = vertex_swarm_peer::SwarmPeerWire {
        multiaddrs_bytes: &p.multiaddrs,
        signature,
        overlay: peer_overlay,
        nonce,
        timestamp,
        chequebook_bytes: &p.chequebook_address,
    };
    // No skew check at the validation layer; that's a session policy.
    let peer = SwarmPeer::parse(wire, network_id, None)?;

    if !peer
        .multiaddrs()
        .iter()
        .all(|addr| extract_peer_id(addr).is_some())
    {
        warn!(
            overlay = %overlay,
            "rejecting peer: multiaddrs missing /p2p/ component"
        );
        return Err(ValidationFailure::MissingPeerId);
    }

    // Store validated peer in LRU cache.
    cache.lock().insert(peer_overlay, peer.clone());

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

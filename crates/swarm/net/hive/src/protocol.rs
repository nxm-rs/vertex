//! Inbound and outbound protocol handlers for hive peer exchange.

use std::sync::Arc;

use std::time::Instant;

use alloy_primitives::{B256, Signature};
use futures::future::BoxFuture;
use libp2p::multiaddr::Protocol;
use metrics::histogram;
use tracing::{debug, warn};
use vertex_net_codec::FramedProto;
use vertex_swarm_api::{SwarmIdentity, SwarmSpec};
use vertex_swarm_net_headers::{
    HeaderedInbound, HeaderedOutbound, HeaderedStream, Inbound, Outbound, ProtocolStreamError,
};
use vertex_swarm_peer::{SwarmAddress, SwarmPeer};

use crate::codec::encode_peers;
use crate::metrics::{HiveMetrics, ValidationFailure};
use crate::PROTOCOL_NAME;

/// 32 KiB frame limit (fits ~100 peers at typical size).
const MAX_MESSAGE_SIZE: usize = 32 * 1024;

type Framed = FramedProto<MAX_MESSAGE_SIZE>;

/// Result of inbound peer validation.
#[derive(Debug)]
pub struct ValidatedPeers {
    pub peers: Vec<SwarmPeer>,
}

/// Inbound handler that receives and validates peers.
#[derive(Debug)]
pub struct HiveInner<I: SwarmIdentity> {
    identity: Arc<I>,
}

impl<I: SwarmIdentity> HiveInner<I> {
    pub fn new(identity: Arc<I>) -> Self {
        Self { identity }
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
        Box::pin(async move {
            use vertex_swarm_net_proto::hive::Peers;

            let mut metrics = HiveMetrics::inbound();

            debug!(network_id, "Hive: reading peers");
            let (proto, _) =
                match Framed::recv::<Peers, ProtocolStreamError, _>(stream.into_inner()).await {
                    Ok(result) => result,
                    Err(e) => {
                        metrics.record_error(&e);
                        return Err(e);
                    }
                };

            let validation_start = Instant::now();
            let peers: Vec<SwarmPeer> = proto
                .peers
                .into_iter()
                .filter_map(|p| {
                    validate_proto_peer(p, network_id, &local_overlay, &mut metrics)
                })
                .collect();
            histogram!("hive_validation_duration_seconds", "direction" => "inbound")
                .record(validation_start.elapsed().as_secs_f64());

            metrics.add_valid_peers(peers.len() as u64);
            metrics.record_success();

            Ok(ValidatedPeers { peers })
        })
    }
}

/// Validate and convert proto peer with metrics tracking.
fn validate_proto_peer(
    p: vertex_swarm_net_proto::hive::Peer,
    network_id: u64,
    local_overlay: &SwarmAddress,
    metrics: &mut HiveMetrics,
) -> Option<SwarmPeer> {
    let overlay = if p.overlay.len() == 32 {
        B256::from_slice(&p.overlay)
    } else {
        debug!(len = p.overlay.len(), "invalid overlay length");
        metrics.record_validation_failure(ValidationFailure::OverlayLength);
        return None;
    };

    // Reject our own overlay address to prevent self-dial
    let peer_overlay = SwarmAddress::from(overlay);
    if peer_overlay == *local_overlay {
        debug!("Hive: rejected self-overlay from peer exchange");
        metrics.record_validation_failure(ValidationFailure::SelfOverlay);
        return None;
    }

    let signature = match Signature::try_from(p.signature.as_slice()) {
        Ok(s) => s,
        Err(e) => {
            debug!(error = %e, "invalid signature format");
            metrics.record_validation_failure(ValidationFailure::SignatureFormat);
            return None;
        }
    };

    let nonce = if p.nonce.len() == 32 {
        B256::from_slice(&p.nonce)
    } else {
        debug!(len = p.nonce.len(), "invalid nonce length");
        metrics.record_validation_failure(ValidationFailure::NonceLength);
        return None;
    };

    // NOTE: validate_overlay disabled due to Bee multiaddr re-serialization bug
    match SwarmPeer::from_signed(
        &p.multiaddrs,
        signature,
        peer_overlay,
        nonce,
        network_id,
        false,
    ) {
        Ok(peer) => {
            if !peer.multiaddrs().iter().all(has_p2p_component) {
                warn!(
                    overlay = %overlay,
                    "rejecting peer: multiaddrs missing /p2p/ component"
                );
                metrics.record_validation_failure(ValidationFailure::MissingPeerId);
                return None;
            }
            Some(peer)
        }
        Err(e) => {
            debug!(overlay = %overlay, error = %e, "peer validation failed");
            metrics.record_validation_failure(ValidationFailure::PeerValidation);
            None
        }
    }
}

fn has_p2p_component(addr: &libp2p::Multiaddr) -> bool {
    addr.iter().any(|p| matches!(p, Protocol::P2p(_)))
}

/// Outbound handler that sends peers to remote.
#[derive(Debug, Clone)]
pub struct HiveOutboundInner {
    proto: vertex_swarm_net_proto::hive::Peers,
    peer_count: usize,
}

impl HiveOutboundInner {
    pub fn new(peers: &[SwarmPeer]) -> Self {
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
        let peer_count = self.peer_count as u64;
        Box::pin(async move {
            let mut metrics = HiveMetrics::outbound();
            metrics.add_valid_peers(peer_count);

            debug!(peer_count, "Hive: sending peers");
            if let Err(e) =
                Framed::send::<_, ProtocolStreamError, _>(stream.into_inner(), self.proto).await
            {
                metrics.record_error(&e);
                return Err(e);
            }

            metrics.record_success();
            Ok(())
        })
    }
}

/// Inbound protocol upgrade with header exchange.
pub type HiveInboundProtocol<I> = Inbound<HiveInner<I>>;

/// Outbound protocol upgrade with header exchange.
pub type HiveOutboundProtocol = Outbound<HiveOutboundInner>;

/// Create an inbound protocol handler for receiving peers.
pub fn inbound<I: SwarmIdentity>(identity: Arc<I>) -> HiveInboundProtocol<I> {
    Inbound::new(HiveInner::new(identity))
}

/// Create an outbound protocol handler for sending peers.
pub fn outbound(peers: &[SwarmPeer]) -> HiveOutboundProtocol {
    Outbound::new(HiveOutboundInner::new(peers))
}

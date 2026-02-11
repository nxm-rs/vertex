//! Inbound and outbound protocol handlers for hive peer exchange.

use std::sync::Arc;

use alloy_primitives::{B256, Signature};
use asynchronous_codec::Framed;
use futures::{SinkExt, TryStreamExt, future::BoxFuture};
use libp2p::multiaddr::Protocol;
use tracing::{Instrument, debug, warn};
use vertex_net_headers::{HeaderedInbound, HeaderedOutbound, HeaderedStream, Inbound, Outbound};
use vertex_swarm_api::{SwarmIdentity, SwarmSpec};
use vertex_swarm_peer::{SwarmAddress, SwarmPeer};

use crate::{
    PROTOCOL_NAME,
    codec::{HiveCodec, Peers},
    error::HiveError,
    metrics::{HiveMetrics, ValidationFailure},
};

/// 32 KiB frame limit (fits ~100 peers at typical size).
const MAX_MESSAGE_SIZE: usize = 32 * 1024;

/// Result of inbound peer validation.
#[derive(Debug)]
pub struct ValidatedPeers {
    /// Peers that passed signature and format validation.
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
    type Error = HiveError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn read(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        let network_id = self.identity.spec().network_id();
        let local_overlay = self.identity.overlay_address();
        let span = tracing::info_span!("hive_receive", network_id = network_id);
        Box::pin(
            async move {
                let mut metrics = HiveMetrics::inbound();

                let codec = HiveCodec::new(MAX_MESSAGE_SIZE);
                let mut framed = Framed::new(stream.into_inner(), codec);

                debug!("Hive: reading peers");
                let inbound = match framed.try_next().await {
                    Ok(Some(msg)) => msg,
                    Ok(None) => {
                        let err = HiveError::ConnectionClosed;
                        metrics.record_error(&err);
                        return Err(err);
                    }
                    Err(e) => {
                        metrics.record_error(&e);
                        return Err(e);
                    }
                };

                let proto_peers = inbound.into_proto_peers();
                let peers: Vec<SwarmPeer> = proto_peers
                    .into_iter()
                    .filter_map(|p| {
                        proto_to_swarm_peer_with_metrics(p, network_id, &local_overlay, &mut metrics)
                    })
                    .collect();

                metrics.add_valid_peers(peers.len() as u64);
                metrics.record_success();

                Ok(ValidatedPeers { peers })
            }
            .instrument(span),
        )
    }
}

/// Validate and convert proto peer with metrics tracking.
///
/// Filters out our own overlay address to prevent self-dial attempts.
fn proto_to_swarm_peer_with_metrics(
    p: crate::proto::hive::Peer,
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

    match SwarmPeer::from_signed(
        &p.multiaddrs,
        signature,
        peer_overlay,
        nonce,
        network_id,
        true,
    ) {
        Ok(peer) => {
            // Validate that all multiaddrs contain /p2p/ component
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

/// Check if a multiaddr contains a /p2p/ component.
fn has_p2p_component(addr: &libp2p::Multiaddr) -> bool {
    addr.iter().any(|p| matches!(p, Protocol::P2p(_)))
}

/// Outbound handler that sends peers to remote.
#[derive(Debug, Clone)]
pub struct HiveOutboundInner {
    peers: Peers,
}

impl HiveOutboundInner {
    pub fn new(peers: &[SwarmPeer]) -> Self {
        Self {
            peers: Peers::from_swarm_peers(peers),
        }
    }
}

impl HeaderedOutbound for HiveOutboundInner {
    type Output = ();
    type Error = HiveError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn write(
        self,
        stream: HeaderedStream,
    ) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        let peer_count = self.peers.len() as u64;
        let span = tracing::info_span!("hive_send", peer_count = peer_count);
        Box::pin(
            async move {
                let mut metrics = HiveMetrics::outbound();
                metrics.add_valid_peers(peer_count);

                let codec = HiveCodec::new(MAX_MESSAGE_SIZE);
                let mut framed = Framed::new(stream.into_inner(), codec);

                debug!("Hive: sending {} peers", peer_count);
                if let Err(e) = framed.send(self.peers).await {
                    metrics.record_error(&e);
                    return Err(e);
                }

                metrics.record_success();
                Ok(())
            }
            .instrument(span),
        )
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

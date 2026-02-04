//! Inbound and outbound protocol handlers for hive peer exchange.

use std::sync::Arc;

use alloy_primitives::{B256, Signature};
use asynchronous_codec::Framed;
use futures::{SinkExt, TryStreamExt, future::BoxFuture};
use tracing::debug;
use vertex_net_headers::{HeaderedInbound, HeaderedOutbound, HeaderedStream, Inbound, Outbound};
use vertex_swarm_api::{SwarmIdentity, SwarmSpec};
use vertex_swarm_peer::{SwarmAddress, SwarmPeer};

use crate::{
    PROTOCOL_NAME,
    codec::{HiveCodec, HiveCodecError, Peers},
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
    type Error = HiveCodecError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn read(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        let network_id = self.identity.spec().network_id();
        Box::pin(async move {
            let codec = HiveCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(stream.into_inner(), codec);

            debug!("Hive: reading peers");
            let inbound = framed.try_next().await?.ok_or_else(|| {
                HiveCodecError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed",
                ))
            })?;

            let proto_peers = inbound.into_proto_peers();
            let total = proto_peers.len();
            let peers: Vec<SwarmPeer> = proto_peers
                .into_iter()
                .filter_map(|p| proto_to_swarm_peer(p, network_id))
                .collect();

            let filtered = total - peers.len();
            if filtered > 0 {
                debug!(
                    total,
                    valid = peers.len(),
                    filtered,
                    "filtered invalid peers"
                );
            }

            Ok(ValidatedPeers { peers })
        })
    }
}

/// Validate and convert proto peer, returning None if invalid (for filter_map).
fn proto_to_swarm_peer(p: crate::proto::hive::Peer, network_id: u64) -> Option<SwarmPeer> {
    let overlay = if p.overlay.len() == 32 {
        B256::from_slice(&p.overlay)
    } else {
        debug!(len = p.overlay.len(), "invalid overlay length");
        return None;
    };

    let signature = match Signature::try_from(p.signature.as_slice()) {
        Ok(s) => s,
        Err(e) => {
            debug!(error = %e, "invalid signature");
            return None;
        }
    };

    let nonce = if p.nonce.len() == 32 {
        B256::from_slice(&p.nonce)
    } else {
        debug!(len = p.nonce.len(), "invalid nonce length");
        return None;
    };

    match SwarmPeer::from_signed(
        &p.multiaddrs,
        signature,
        SwarmAddress::from(overlay),
        nonce,
        network_id,
        true,
    ) {
        Ok(peer) => Some(peer),
        Err(e) => {
            debug!(overlay = %overlay, error = %e, "peer validation failed");
            None
        }
    }
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
    type Error = HiveCodecError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn write(
        self,
        stream: HeaderedStream,
    ) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        Box::pin(async move {
            let codec = HiveCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(stream.into_inner(), codec);

            debug!("Hive: sending peers");
            framed.send(self.peers).await?;
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

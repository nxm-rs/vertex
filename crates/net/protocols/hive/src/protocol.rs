//! Protocol upgrade for hive.

use alloy_primitives::{B256, Signature};
use asynchronous_codec::Framed;
use futures::{SinkExt, TryStreamExt, future::BoxFuture};
use tracing::debug;
use vertex_swarm_peer::{SwarmAddress, SwarmPeer};
use vertex_net_headers::{HeaderedInbound, HeaderedOutbound, HeaderedStream, Inbound, Outbound};
use vertex_swarm_api::SwarmNodeTypes;
use vertex_swarmspec::SwarmSpec;

use crate::{
    PROTOCOL_NAME,
    codec::{HiveCodec, HiveCodecError, Peers},
};

const MAX_MESSAGE_SIZE: usize = 32 * 1024;

/// Validated peers from hive inbound.
#[derive(Debug)]
pub struct ValidatedPeers {
    pub peers: Vec<SwarmPeer>,
}

/// Hive inbound: receives and validates peers.
#[derive(Debug, Clone)]
pub struct HiveInner<N: SwarmNodeTypes> {
    spec: N::Spec,
}

impl<N: SwarmNodeTypes> HiveInner<N> {
    pub fn new(spec: N::Spec) -> Self {
        Self { spec }
    }
}

impl<N: SwarmNodeTypes> HeaderedInbound for HiveInner<N> {
    type Output = ValidatedPeers;
    type Error = HiveCodecError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn read(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        let network_id = self.spec.network_id();
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
                debug!(total, valid = peers.len(), filtered, "filtered invalid peers");
            }

            Ok(ValidatedPeers { peers })
        })
    }
}

/// Convert proto peer to SwarmPeer with validation.
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

/// Hive outbound: sends peers.
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

pub type HiveInboundProtocol<N> = Inbound<HiveInner<N>>;
pub type HiveOutboundProtocol = Outbound<HiveOutboundInner>;

pub fn inbound<N: SwarmNodeTypes>(spec: &N::Spec) -> HiveInboundProtocol<N> {
    Inbound::new(HiveInner::new(spec.clone()))
}

pub fn outbound(peers: &[SwarmPeer]) -> HiveOutboundProtocol {
    Outbound::new(HiveOutboundInner::new(peers))
}

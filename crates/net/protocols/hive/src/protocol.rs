//! Protocol upgrade for hive.
//!
//! Implements HeaderedInbound/HeaderedOutbound traits - headers are automatic.

use asynchronous_codec::Framed;
use futures::{SinkExt, TryStreamExt, future::BoxFuture};
use tracing::debug;
use vertex_net_headers::{HeaderedInbound, HeaderedOutbound, HeaderedStream, Inbound, Outbound};
use vertex_net_primitives::validate_bzz_address;
use vertex_node_types::NodeTypes;
use vertex_swarmspec::SwarmSpec;

use crate::{
    PROTOCOL_NAME,
    codec::{BzzAddress, HiveCodec, HiveCodecError, Peers},
};

const MAX_MESSAGE_SIZE: usize = 32 * 1024;

/// Hive inbound: receives peers from remote and validates them.
///
/// Only peers with valid signatures and correct overlay derivation
/// for this network are returned. Invalid peers are logged at debug level.
#[derive(Debug, Clone)]
pub struct HiveInner<N: NodeTypes> {
    spec: N::Spec,
}

impl<N: NodeTypes> HiveInner<N> {
    /// Create a new hive inbound handler.
    pub fn new(spec: N::Spec) -> Self {
        Self { spec }
    }
}

impl<N: NodeTypes> HeaderedInbound for HiveInner<N> {
    type Output = Peers;
    type Error = HiveCodecError;

    fn protocol_name(&self) -> &'static str {
        PROTOCOL_NAME
    }

    fn read(self, stream: HeaderedStream) -> BoxFuture<'static, Result<Self::Output, Self::Error>> {
        let network_id = self.spec.network_id();
        Box::pin(async move {
            let codec = HiveCodec::new(MAX_MESSAGE_SIZE);
            let mut framed = Framed::new(stream.into_inner(), codec);

            debug!("Hive: Reading peers message");
            let peers = framed.try_next().await?.ok_or_else(|| {
                HiveCodecError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed",
                ))
            })?;

            let total_count = peers.peers.len();
            let valid_peers: Vec<BzzAddress> = peers
                .peers
                .into_iter()
                .filter(|peer| {
                    match validate_bzz_address(
                        &peer.underlays,
                        &peer.overlay,
                        &peer.signature,
                        &peer.nonce,
                        network_id,
                    ) {
                        Ok(()) => true,
                        Err(e) => {
                            debug!(
                                overlay = %peer.overlay,
                                error = %e,
                                "Hive: filtering invalid peer"
                            );
                            false
                        }
                    }
                })
                .collect();

            let filtered_count = total_count - valid_peers.len();
            if filtered_count > 0 {
                debug!(
                    total = total_count,
                    valid = valid_peers.len(),
                    filtered = filtered_count,
                    "Hive: filtered invalid peers"
                );
            }

            Ok(Peers::new(valid_peers))
        })
    }
}

/// Hive outbound: sends peers to remote.
#[derive(Debug, Clone)]
pub struct HiveOutboundInner {
    peers: Peers,
}

impl HiveOutboundInner {
    pub fn new(peers: Peers) -> Self {
        Self { peers }
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

            debug!("Hive: Sending peers message");
            framed.send(self.peers).await?;
            Ok(())
        })
    }
}

// Type aliases for handler
pub type HiveInboundProtocol<N> = Inbound<HiveInner<N>>;
pub type HiveOutboundProtocol = Outbound<HiveOutboundInner>;

pub fn inbound<N: NodeTypes>(spec: &N::Spec) -> HiveInboundProtocol<N> {
    Inbound::new(HiveInner::new(spec.clone()))
}

pub fn outbound(peers: Peers) -> HiveOutboundProtocol {
    Outbound::new(HiveOutboundInner::new(peers))
}

//! Combined protocol upgrades for topology handler.
//!
//! This module provides multi-protocol support for the topology handler,
//! combining handshake, hive, and pingpong into a single `InboundUpgrade`.
//!
//! # Architecture
//!
//! The topology handler needs to accept multiple inbound protocols:
//! - Handshake: Must complete before other protocols
//! - Hive: Peer discovery gossip (after handshake)
//! - Pingpong: Connection liveness (after handshake)
//!
//! We use a custom `TopologyInboundUpgrade` that implements `UpgradeInfo`
//! with all protocol names and dispatches based on the negotiated protocol.

use std::sync::Arc;

use futures::future::BoxFuture;
use libp2p::{InboundUpgrade, Multiaddr, OutboundUpgrade, PeerId, Stream, core::UpgradeInfo};
use thiserror::Error;
use vertex_net_handshake::{
    HandshakeError, HandshakeInfo, HandshakeProtocol, PROTOCOL as HANDSHAKE_PROTOCOL,
};
use vertex_net_headers::ProtocolError;
use vertex_net_hive::{HiveOutboundProtocol, PROTOCOL_NAME as HIVE_PROTOCOL, Peers};
use vertex_net_pingpong::{
    PROTOCOL_NAME as PINGPONG_PROTOCOL, PingpongInboundProtocol, PingpongOutboundProtocol, Pong,
};
use vertex_node_types::{Identity, NodeTypes};

// ============================================================================
// Error Types
// ============================================================================

/// Errors from topology protocol upgrades.
#[derive(Debug, Error)]
pub enum TopologyUpgradeError {
    /// Handshake protocol error.
    #[error("handshake error: {0}")]
    Handshake(#[from] HandshakeError),

    /// Hive protocol error.
    #[error("hive error: {0}")]
    Hive(#[source] ProtocolError),

    /// Pingpong protocol error.
    #[error("pingpong error: {0}")]
    Pingpong(#[source] ProtocolError),

    /// Unknown protocol negotiated.
    #[error("unknown protocol: {0}")]
    UnknownProtocol(String),
}

// ============================================================================
// Inbound Protocol
// ============================================================================

/// Output from a topology inbound upgrade.
#[derive(Debug)]
pub enum TopologyInboundOutput {
    /// Handshake completed successfully.
    Handshake(HandshakeInfo),
    /// Received peers via hive.
    Hive(Peers),
    /// Responded to a ping.
    Pingpong,
}

/// Combined inbound upgrade for topology protocols.
///
/// Advertises handshake, hive, and pingpong protocols and dispatches
/// to the appropriate handler based on the negotiated protocol.
///
/// Generic over `N: NodeTypes` to support different node configurations.
#[derive(Clone)]
pub struct TopologyInboundUpgrade<N: NodeTypes> {
    identity: Arc<N::Identity>,
    peer_id: PeerId,
    remote_addr: Multiaddr,
}

impl<N: NodeTypes> TopologyInboundUpgrade<N> {
    /// Create a new topology inbound upgrade.
    pub fn new(identity: Arc<N::Identity>, peer_id: PeerId, remote_addr: Multiaddr) -> Self {
        Self {
            identity,
            peer_id,
            remote_addr,
        }
    }
}

impl<N: NodeTypes> std::fmt::Debug for TopologyInboundUpgrade<N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TopologyInboundUpgrade")
            .field("peer_id", &self.peer_id)
            .field("remote_addr", &self.remote_addr)
            .finish_non_exhaustive()
    }
}

impl<N: NodeTypes> UpgradeInfo for TopologyInboundUpgrade<N> {
    type Info = &'static str;
    type InfoIter = std::vec::IntoIter<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        vec![HANDSHAKE_PROTOCOL, HIVE_PROTOCOL, PINGPONG_PROTOCOL].into_iter()
    }
}

impl<N: NodeTypes> InboundUpgrade<Stream> for TopologyInboundUpgrade<N> {
    type Output = TopologyInboundOutput;
    type Error = TopologyUpgradeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, socket: Stream, info: Self::Info) -> Self::Future {
        Box::pin(async move {
            match info {
                HANDSHAKE_PROTOCOL => {
                    let handshake =
                        HandshakeProtocol::<N>::new(self.identity, self.peer_id, self.remote_addr);
                    let result = handshake.upgrade_inbound(socket, info).await?;
                    Ok(TopologyInboundOutput::Handshake(result))
                }
                HIVE_PROTOCOL => {
                    let hive = vertex_net_hive::inbound::<N>(self.identity.spec());
                    let peers = hive
                        .upgrade_inbound(socket, info)
                        .await
                        .map_err(TopologyUpgradeError::Hive)?;
                    Ok(TopologyInboundOutput::Hive(peers))
                }
                PINGPONG_PROTOCOL => {
                    let pingpong: PingpongInboundProtocol = vertex_net_pingpong::inbound();
                    pingpong
                        .upgrade_inbound(socket, info)
                        .await
                        .map_err(TopologyUpgradeError::Pingpong)?;
                    Ok(TopologyInboundOutput::Pingpong)
                }
                other => Err(TopologyUpgradeError::UnknownProtocol(other.to_string())),
            }
        })
    }
}

// ============================================================================
// Outbound Protocol
// ============================================================================

/// Type of outbound request for topology.
#[derive(Debug, Clone)]
pub enum TopologyOutboundRequest {
    /// Initiate handshake.
    Handshake,
    /// Broadcast peers via hive.
    Hive(Peers),
    /// Send a ping with greeting.
    Pingpong(String),
}

/// Output from a topology outbound upgrade.
#[derive(Debug)]
pub enum TopologyOutboundOutput {
    /// Handshake completed.
    Handshake(HandshakeInfo),
    /// Hive broadcast completed.
    Hive,
    /// Pong received.
    Pingpong(Pong),
}

/// Combined outbound upgrade for topology protocols.
///
/// Unlike inbound, outbound requests know which protocol to use.
/// This enum wraps the specific request type.
///
/// Generic over `N: NodeTypes` to support different node configurations.
#[derive(Clone)]
pub struct TopologyOutboundUpgrade<N: NodeTypes> {
    identity: Arc<N::Identity>,
    peer_id: PeerId,
    remote_addr: Multiaddr,
    request: TopologyOutboundRequest,
}

impl<N: NodeTypes> TopologyOutboundUpgrade<N> {
    /// Create a new handshake outbound upgrade.
    pub fn handshake(identity: Arc<N::Identity>, peer_id: PeerId, remote_addr: Multiaddr) -> Self {
        Self {
            identity,
            peer_id,
            remote_addr,
            request: TopologyOutboundRequest::Handshake,
        }
    }

    /// Create a new hive outbound upgrade.
    pub fn hive(
        identity: Arc<N::Identity>,
        peer_id: PeerId,
        remote_addr: Multiaddr,
        peers: Peers,
    ) -> Self {
        Self {
            identity,
            peer_id,
            remote_addr,
            request: TopologyOutboundRequest::Hive(peers),
        }
    }

    /// Create a new pingpong outbound upgrade.
    pub fn pingpong(
        identity: Arc<N::Identity>,
        peer_id: PeerId,
        remote_addr: Multiaddr,
        greeting: String,
    ) -> Self {
        Self {
            identity,
            peer_id,
            remote_addr,
            request: TopologyOutboundRequest::Pingpong(greeting),
        }
    }

    /// Get the protocol name for this request.
    fn protocol_name(&self) -> &'static str {
        match &self.request {
            TopologyOutboundRequest::Handshake => HANDSHAKE_PROTOCOL,
            TopologyOutboundRequest::Hive(_) => HIVE_PROTOCOL,
            TopologyOutboundRequest::Pingpong(_) => PINGPONG_PROTOCOL,
        }
    }
}

impl<N: NodeTypes> std::fmt::Debug for TopologyOutboundUpgrade<N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TopologyOutboundUpgrade")
            .field("peer_id", &self.peer_id)
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

impl<N: NodeTypes> UpgradeInfo for TopologyOutboundUpgrade<N> {
    type Info = &'static str;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(self.protocol_name())
    }
}

impl<N: NodeTypes> OutboundUpgrade<Stream> for TopologyOutboundUpgrade<N> {
    type Output = TopologyOutboundOutput;
    type Error = TopologyUpgradeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_outbound(self, socket: Stream, info: Self::Info) -> Self::Future {
        Box::pin(async move {
            match self.request {
                TopologyOutboundRequest::Handshake => {
                    let handshake =
                        HandshakeProtocol::<N>::new(self.identity, self.peer_id, self.remote_addr);
                    let result = handshake.upgrade_outbound(socket, info).await?;
                    Ok(TopologyOutboundOutput::Handshake(result))
                }
                TopologyOutboundRequest::Hive(peers) => {
                    let hive: HiveOutboundProtocol = vertex_net_hive::outbound(peers);
                    hive.upgrade_outbound(socket, info)
                        .await
                        .map_err(TopologyUpgradeError::Hive)?;
                    Ok(TopologyOutboundOutput::Hive)
                }
                TopologyOutboundRequest::Pingpong(greeting) => {
                    let pingpong: PingpongOutboundProtocol =
                        vertex_net_pingpong::outbound(greeting);
                    let pong = pingpong
                        .upgrade_outbound(socket, info)
                        .await
                        .map_err(TopologyUpgradeError::Pingpong)?;
                    Ok(TopologyOutboundOutput::Pingpong(pong))
                }
            }
        })
    }
}

// ============================================================================
// Info for Tracking Outbound Requests
// ============================================================================

/// Information about an outbound request, used for correlating responses.
#[derive(Debug, Clone)]
pub enum TopologyOutboundInfo {
    /// Handshake request.
    Handshake,
    /// Hive broadcast.
    Hive,
    /// Ping request with sent timestamp.
    Pingpong { sent_at: std::time::Instant },
}

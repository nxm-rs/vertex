//! Protocol upgrades for topology handler.
//!
//! Provides multi-protocol support combining handshake, hive, and pingpong into
//! unified inbound/outbound upgrades for the connection handler.

use std::sync::Arc;

use futures::future::BoxFuture;
use libp2p::{InboundUpgrade, Multiaddr, OutboundUpgrade, PeerId, Stream, core::UpgradeInfo};
use tracing::debug;
use vertex_net_handshake::{
    HandshakeError, HandshakeInfo, HandshakeProtocol, PROTOCOL as HANDSHAKE_PROTOCOL,
};
use vertex_net_headers::ProtocolError;
use vertex_net_hive::{HiveOutboundProtocol, PROTOCOL_NAME as HIVE_PROTOCOL};
use vertex_net_pingpong::{
    PROTOCOL_NAME as PINGPONG_PROTOCOL, PingpongInboundProtocol, PingpongOutboundProtocol, Pong,
};
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_peer::SwarmPeer;

use crate::nat_discovery::NatDiscovery;

/// Errors from topology protocol upgrades.
#[derive(Debug, thiserror::Error)]
pub enum TopologyUpgradeError {
    #[error("handshake error: {0}")]
    Handshake(#[from] HandshakeError),

    #[error("hive error: {0}")]
    Hive(#[source] ProtocolError),

    #[error("pingpong error: {0}")]
    Pingpong(#[source] ProtocolError),

    #[error("unknown protocol: {0}")]
    UnknownProtocol(String),
}

/// Output from an inbound topology upgrade.
pub enum TopologyInboundOutput {
    Handshake(Box<HandshakeInfo>),
    Hive(Vec<SwarmPeer>),
    Pingpong,
}

/// Inbound upgrade that handles handshake, hive, and pingpong protocols.
#[derive(Clone)]
pub struct TopologyInboundUpgrade<I: SwarmIdentity> {
    identity: Arc<I>,
    peer_id: PeerId,
    remote_addr: Multiaddr,
    nat_discovery: Arc<NatDiscovery>,
}

impl<I: SwarmIdentity> TopologyInboundUpgrade<I> {
    pub fn new(
        identity: Arc<I>,
        peer_id: PeerId,
        remote_addr: Multiaddr,
        nat_discovery: Arc<NatDiscovery>,
    ) -> Self {
        Self {
            identity,
            peer_id,
            remote_addr,
            nat_discovery,
        }
    }
}

impl<I: SwarmIdentity> std::fmt::Debug for TopologyInboundUpgrade<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TopologyInboundUpgrade")
            .field("peer_id", &self.peer_id)
            .field("remote_addr", &self.remote_addr)
            .finish_non_exhaustive()
    }
}

impl<I: SwarmIdentity> UpgradeInfo for TopologyInboundUpgrade<I> {
    type Info = &'static str;
    type InfoIter = std::vec::IntoIter<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        vec![HANDSHAKE_PROTOCOL, HIVE_PROTOCOL, PINGPONG_PROTOCOL].into_iter()
    }
}

impl<I: SwarmIdentity> InboundUpgrade<Stream> for TopologyInboundUpgrade<I> {
    type Output = TopologyInboundOutput;
    type Error = TopologyUpgradeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, socket: Stream, info: Self::Info) -> Self::Future {
        Box::pin(async move {
            match info {
                HANDSHAKE_PROTOCOL => {
                    let additional_addrs = self.nat_discovery.addresses_for_peer(&self.remote_addr);

                    debug!(
                        peer_id = %self.peer_id,
                        remote_addr = %self.remote_addr,
                        additional_addrs_count = additional_addrs.len(),
                        "Inbound handshake: selected addresses for peer"
                    );

                    let handshake = HandshakeProtocol::with_addrs(
                        Arc::clone(&self.identity),
                        self.peer_id,
                        self.remote_addr.clone(),
                        additional_addrs,
                    );
                    let result = handshake.upgrade_inbound(socket, info).await?;

                    debug!(
                        peer_id = %self.peer_id,
                        observed_addr = %result.observed_multiaddr(),
                        "Inbound handshake: reporting observed address"
                    );
                    self.nat_discovery.on_observed_addr(
                        result.observed_multiaddr().clone(),
                        &self.remote_addr,
                    );

                    Ok(TopologyInboundOutput::Handshake(Box::new(result)))
                }
                HIVE_PROTOCOL => {
                    let hive = vertex_net_hive::inbound(Arc::clone(&self.identity));
                    let validated = hive
                        .upgrade_inbound(socket, info)
                        .await
                        .map_err(TopologyUpgradeError::Hive)?;
                    Ok(TopologyInboundOutput::Hive(validated.peers))
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

/// Type of outbound request.
#[derive(Debug, Clone)]
pub(crate) enum TopologyOutboundRequest {
    Handshake,
    Hive(Vec<SwarmPeer>),
    Pingpong(String),
}

/// Output from an outbound topology upgrade.
pub enum TopologyOutboundOutput {
    Handshake(Box<HandshakeInfo>),
    Hive,
    Pingpong(Pong),
}

/// Outbound upgrade for a specific topology protocol.
#[derive(Clone)]
pub struct TopologyOutboundUpgrade<I: SwarmIdentity> {
    identity: Arc<I>,
    peer_id: PeerId,
    remote_addr: Multiaddr,
    request: TopologyOutboundRequest,
    nat_discovery: Arc<NatDiscovery>,
}

impl<I: SwarmIdentity> TopologyOutboundUpgrade<I> {
    pub fn handshake(
        identity: Arc<I>,
        peer_id: PeerId,
        remote_addr: Multiaddr,
        nat_discovery: Arc<NatDiscovery>,
    ) -> Self {
        Self {
            identity,
            peer_id,
            remote_addr,
            request: TopologyOutboundRequest::Handshake,
            nat_discovery,
        }
    }

    pub fn hive(
        identity: Arc<I>,
        peer_id: PeerId,
        remote_addr: Multiaddr,
        peers: Vec<SwarmPeer>,
        nat_discovery: Arc<NatDiscovery>,
    ) -> Self {
        Self {
            identity,
            peer_id,
            remote_addr,
            request: TopologyOutboundRequest::Hive(peers),
            nat_discovery,
        }
    }

    pub fn pingpong(
        identity: Arc<I>,
        peer_id: PeerId,
        remote_addr: Multiaddr,
        greeting: String,
        nat_discovery: Arc<NatDiscovery>,
    ) -> Self {
        Self {
            identity,
            peer_id,
            remote_addr,
            request: TopologyOutboundRequest::Pingpong(greeting),
            nat_discovery,
        }
    }

    fn protocol_name(&self) -> &'static str {
        match &self.request {
            TopologyOutboundRequest::Handshake => HANDSHAKE_PROTOCOL,
            TopologyOutboundRequest::Hive(_) => HIVE_PROTOCOL,
            TopologyOutboundRequest::Pingpong(_) => PINGPONG_PROTOCOL,
        }
    }
}

impl<I: SwarmIdentity> std::fmt::Debug for TopologyOutboundUpgrade<I> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TopologyOutboundUpgrade")
            .field("peer_id", &self.peer_id)
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

impl<I: SwarmIdentity> UpgradeInfo for TopologyOutboundUpgrade<I> {
    type Info = &'static str;
    type InfoIter = std::iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(self.protocol_name())
    }
}

impl<I: SwarmIdentity> OutboundUpgrade<Stream> for TopologyOutboundUpgrade<I> {
    type Output = TopologyOutboundOutput;
    type Error = TopologyUpgradeError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_outbound(self, socket: Stream, info: Self::Info) -> Self::Future {
        Box::pin(async move {
            match self.request {
                TopologyOutboundRequest::Handshake => {
                    let additional_addrs = self.nat_discovery.addresses_for_peer(&self.remote_addr);

                    debug!(
                        peer_id = %self.peer_id,
                        remote_addr = %self.remote_addr,
                        additional_addrs_count = additional_addrs.len(),
                        "Outbound handshake: selected addresses for peer"
                    );

                    let handshake = HandshakeProtocol::with_addrs(
                        Arc::clone(&self.identity),
                        self.peer_id,
                        self.remote_addr.clone(),
                        additional_addrs,
                    );
                    let result = handshake.upgrade_outbound(socket, info).await?;

                    debug!(
                        peer_id = %self.peer_id,
                        observed_addr = %result.observed_multiaddr(),
                        "Outbound handshake: reporting observed address"
                    );
                    self.nat_discovery.on_observed_addr(
                        result.observed_multiaddr().clone(),
                        &self.remote_addr,
                    );

                    Ok(TopologyOutboundOutput::Handshake(Box::new(result)))
                }
                TopologyOutboundRequest::Hive(peers) => {
                    let hive: HiveOutboundProtocol = vertex_net_hive::outbound(&peers);
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

/// Info for tracking outbound requests.
#[derive(Debug, Clone)]
pub enum TopologyOutboundInfo {
    Handshake,
    Hive,
    Pingpong { sent_at: std::time::Instant },
}

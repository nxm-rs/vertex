//! Per-connection handler for topology protocols.
//!
//! Manages handshake, hive, and pingpong on a single connection. The handler
//! starts in `Handshaking` state and transitions to `Ready` after successful
//! handshake, at which point hive and pingpong become available.
//!
//! The internal [`Command`] and [`Event`] types are used for communication
//! between the handler and [`TopologyBehaviour`](crate::TopologyBehaviour).
//! External code should use [`TopologyCommand`](crate::TopologyCommand) and
//! [`TopologyEvent`](crate::TopologyEvent) instead.

use std::{
    collections::VecDeque,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use libp2p::{
    Multiaddr, PeerId,
    swarm::{
        SubstreamProtocol,
        handler::{
            ConnectionEvent, ConnectionHandler, ConnectionHandlerEvent, FullyNegotiatedInbound,
            FullyNegotiatedOutbound,
        },
    },
};
use tracing::{debug, trace, warn};
use vertex_net_handshake::{HANDSHAKE_TIMEOUT, HandshakeError, HandshakeInfo};
use vertex_swarm_api::SwarmNodeTypes;
use vertex_swarm_peer::SwarmPeer;
use vertex_swarm_peermanager::AddressManager;

use crate::protocol::{
    TopologyInboundOutput, TopologyInboundUpgrade, TopologyOutboundInfo, TopologyOutboundOutput,
    TopologyOutboundUpgrade,
};

/// Configuration for topology protocols.
#[derive(Debug, Clone)]
pub struct TopologyConfig {
    /// Timeout for hive operations.
    pub hive_timeout: Duration,
    /// Timeout for pingpong operations.
    pub pingpong_timeout: Duration,
    /// Default greeting for pingpong.
    pub pingpong_greeting: String,
}

impl Default for TopologyConfig {
    fn default() -> Self {
        Self {
            hive_timeout: Duration::from_secs(60),
            pingpong_timeout: Duration::from_secs(30),
            pingpong_greeting: "ping".to_string(),
        }
    }
}

/// Commands from behaviour to handler.
///
/// These are internal types - use [`TopologyCommand`](crate::TopologyCommand) instead.
pub enum Command {
    /// Start handshake with resolved address.
    StartHandshake(Multiaddr),
    /// Broadcast peers via hive.
    BroadcastPeers(Vec<SwarmPeer>),
    /// Send a ping.
    Ping { greeting: Option<String> },
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Command::StartHandshake(addr) => f.debug_tuple("StartHandshake").field(addr).finish(),
            Command::BroadcastPeers(peers) => {
                f.debug_tuple("BroadcastPeers").field(&peers.len()).finish()
            }
            Command::Ping { greeting } => {
                f.debug_struct("Ping").field("greeting", greeting).finish()
            }
        }
    }
}

/// Events from handler to behaviour.
///
/// These are internal types - use [`TopologyEvent`](crate::TopologyEvent) instead.
pub enum Event {
    /// Handshake completed.
    HandshakeCompleted(Box<HandshakeInfo>),
    /// Handshake failed.
    HandshakeFailed(HandshakeError),
    /// Received peers via hive.
    HivePeersReceived(Vec<SwarmPeer>),
    /// Hive broadcast completed.
    HiveBroadcastComplete,
    /// Hive error.
    HiveError(String),
    /// Pong received with RTT.
    PingpongPong { rtt: Duration },
    /// Responded to incoming ping.
    PingpongPingReceived,
    /// Pingpong error.
    PingpongError(String),
}

impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HandshakeCompleted(info) => f
                .debug_tuple("HandshakeCompleted")
                .field(&info.peer_id)
                .finish(),
            Self::HandshakeFailed(e) => f.debug_tuple("HandshakeFailed").field(e).finish(),
            Self::HivePeersReceived(peers) => f
                .debug_tuple("HivePeersReceived")
                .field(&peers.len())
                .finish(),
            Self::HiveBroadcastComplete => f.debug_tuple("HiveBroadcastComplete").finish(),
            Self::HiveError(e) => f.debug_tuple("HiveError").field(e).finish(),
            Self::PingpongPong { rtt } => f.debug_struct("PingpongPong").field("rtt", rtt).finish(),
            Self::PingpongPingReceived => f.debug_tuple("PingpongPingReceived").finish(),
            Self::PingpongError(e) => f.debug_tuple("PingpongError").field(e).finish(),
        }
    }
}

#[derive(Debug)]
enum State {
    Handshaking,
    Ready,
    Failed,
}

#[allow(dead_code)]
struct PendingPing {
    sent_at: Instant,
}

/// Per-connection handler for topology protocols.
pub struct TopologyHandler<N: SwarmNodeTypes> {
    config: TopologyConfig,
    identity: N::Identity,
    peer_id: PeerId,
    remote_addr: Multiaddr,
    address_manager: Option<Arc<AddressManager>>,
    state: State,
    pending_events: VecDeque<Event>,
    pending_hive_outbound: VecDeque<Vec<SwarmPeer>>,
    pending_ping: Option<PendingPing>,
    should_initiate_handshake: bool,
    handshake_outbound_pending: bool,
    hive_outbound_pending: bool,
    pingpong_outbound_pending: bool,
    pending_ping_command: Option<String>,
}

impl<N: SwarmNodeTypes> TopologyHandler<N> {
    /// Create a new handler.
    pub fn new(
        config: TopologyConfig,
        identity: N::Identity,
        peer_id: PeerId,
        remote_addr: &Multiaddr,
    ) -> Self {
        Self {
            config,
            identity,
            peer_id,
            remote_addr: remote_addr.clone(),
            address_manager: None,
            state: State::Handshaking,
            pending_events: VecDeque::new(),
            pending_hive_outbound: VecDeque::new(),
            pending_ping: None,
            should_initiate_handshake: false,
            handshake_outbound_pending: false,
            hive_outbound_pending: false,
            pingpong_outbound_pending: false,
            pending_ping_command: None,
        }
    }

    /// Create a handler with address management for smart address selection.
    pub fn with_address_manager(
        config: TopologyConfig,
        identity: N::Identity,
        peer_id: PeerId,
        remote_addr: &Multiaddr,
        address_manager: Arc<AddressManager>,
    ) -> Self {
        Self {
            config,
            identity,
            peer_id,
            remote_addr: remote_addr.clone(),
            address_manager: Some(address_manager),
            state: State::Handshaking,
            pending_events: VecDeque::new(),
            pending_hive_outbound: VecDeque::new(),
            pending_ping: None,
            should_initiate_handshake: false,
            handshake_outbound_pending: false,
            hive_outbound_pending: false,
            pingpong_outbound_pending: false,
            pending_ping_command: None,
        }
    }

    fn inbound_upgrade(&self) -> TopologyInboundUpgrade<N> {
        match &self.address_manager {
            Some(mgr) => TopologyInboundUpgrade::with_address_manager(
                self.identity.clone(),
                self.peer_id,
                self.remote_addr.clone(),
                mgr.clone(),
            ),
            None => TopologyInboundUpgrade::new(
                self.identity.clone(),
                self.peer_id,
                self.remote_addr.clone(),
            ),
        }
    }

    fn is_ready(&self) -> bool {
        matches!(self.state, State::Ready)
    }
}

#[allow(deprecated)]
impl<N: SwarmNodeTypes> ConnectionHandler for TopologyHandler<N> {
    type FromBehaviour = Command;
    type ToBehaviour = Event;
    type InboundProtocol = TopologyInboundUpgrade<N>;
    type OutboundProtocol = TopologyOutboundUpgrade<N>;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = TopologyOutboundInfo;

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        SubstreamProtocol::new(self.inbound_upgrade(), ()).with_timeout(HANDSHAKE_TIMEOUT)
    }

    fn connection_keep_alive(&self) -> bool {
        !matches!(self.state, State::Failed)
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        if let Some(event) = self.pending_events.pop_front() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        // Initiate handshake if requested
        if matches!(self.state, State::Handshaking)
            && self.should_initiate_handshake
            && !self.handshake_outbound_pending
        {
            self.should_initiate_handshake = false;
            self.handshake_outbound_pending = true;
            debug!(peer_id = %self.peer_id, "Initiating outbound handshake");
            let upgrade = match &self.address_manager {
                Some(mgr) => TopologyOutboundUpgrade::handshake_with_address_manager(
                    self.identity.clone(),
                    self.peer_id,
                    self.remote_addr.clone(),
                    mgr.clone(),
                ),
                None => TopologyOutboundUpgrade::handshake(
                    self.identity.clone(),
                    self.peer_id,
                    self.remote_addr.clone(),
                ),
            };
            return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                protocol: SubstreamProtocol::new(upgrade, TopologyOutboundInfo::Handshake)
                    .with_timeout(HANDSHAKE_TIMEOUT),
            });
        }

        // Process hive broadcasts
        if self.is_ready()
            && !self.hive_outbound_pending
            && let Some(peers) = self.pending_hive_outbound.pop_front()
        {
            self.hive_outbound_pending = true;
            let upgrade = TopologyOutboundUpgrade::hive(
                self.identity.clone(),
                self.peer_id,
                self.remote_addr.clone(),
                peers,
            );
            return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                protocol: SubstreamProtocol::new(upgrade, TopologyOutboundInfo::Hive)
                    .with_timeout(self.config.hive_timeout),
            });
        }

        // Process ping commands
        if self.is_ready()
            && !self.pingpong_outbound_pending
            && let Some(greeting) = self.pending_ping_command.take()
        {
            self.pingpong_outbound_pending = true;
            let sent_at = Instant::now();
            self.pending_ping = Some(PendingPing { sent_at });
            let upgrade = TopologyOutboundUpgrade::pingpong(
                self.identity.clone(),
                self.peer_id,
                self.remote_addr.clone(),
                greeting,
            );
            return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                protocol: SubstreamProtocol::new(
                    upgrade,
                    TopologyOutboundInfo::Pingpong { sent_at },
                )
                .with_timeout(self.config.pingpong_timeout),
            });
        }

        Poll::Pending
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match event {
            Command::StartHandshake(resolved_addr) => {
                if matches!(self.state, State::Handshaking) && !self.handshake_outbound_pending {
                    self.remote_addr = resolved_addr.clone();
                    self.should_initiate_handshake = true;
                    debug!("Handshake will use resolved address: {}", resolved_addr);
                }
            }
            Command::BroadcastPeers(peers) => {
                if self.is_ready() {
                    self.pending_hive_outbound.push_back(peers);
                } else {
                    warn!("Cannot broadcast peers before handshake complete");
                }
            }
            Command::Ping { greeting } => {
                if self.is_ready() && !self.pingpong_outbound_pending {
                    self.pending_ping_command =
                        Some(greeting.unwrap_or_else(|| self.config.pingpong_greeting.clone()));
                } else if !self.is_ready() {
                    warn!("Cannot ping before handshake complete");
                }
            }
        }
    }

    fn on_connection_event(
        &mut self,
        event: ConnectionEvent<
            Self::InboundProtocol,
            Self::OutboundProtocol,
            Self::InboundOpenInfo,
            Self::OutboundOpenInfo,
        >,
    ) {
        match event {
            ConnectionEvent::FullyNegotiatedInbound(FullyNegotiatedInbound {
                protocol: output,
                ..
            }) => {
                self.handle_inbound_output(output);
            }

            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol: output,
                info,
                ..
            }) => {
                self.handle_outbound_output(output, info);
            }

            ConnectionEvent::DialUpgradeError(error) => {
                warn!(peer_id = %self.peer_id, error = %error.error, "Outbound upgrade error");
                match error.info {
                    TopologyOutboundInfo::Handshake => {
                        self.handshake_outbound_pending = false;
                        let handshake_error = HandshakeError::Protocol(error.error.to_string());
                        self.state = State::Failed;
                        self.pending_events
                            .push_back(Event::HandshakeFailed(handshake_error));
                    }
                    TopologyOutboundInfo::Hive => {
                        self.hive_outbound_pending = false;
                        self.pending_events
                            .push_back(Event::HiveError(error.error.to_string()));
                    }
                    TopologyOutboundInfo::Pingpong { .. } => {
                        self.pingpong_outbound_pending = false;
                        self.pending_ping = None;
                        self.pending_events
                            .push_back(Event::PingpongError(error.error.to_string()));
                    }
                }
            }

            ConnectionEvent::ListenUpgradeError(error) => {
                warn!(peer_id = %self.peer_id, error = %error.error, "Inbound upgrade error");
                if matches!(self.state, State::Handshaking) {
                    let handshake_error = HandshakeError::Protocol(error.error.to_string());
                    self.state = State::Failed;
                    self.pending_events
                        .push_back(Event::HandshakeFailed(handshake_error));
                }
            }

            _ => {}
        }
    }
}

impl<N: SwarmNodeTypes> TopologyHandler<N> {
    fn handle_inbound_output(&mut self, output: TopologyInboundOutput) {
        match output {
            TopologyInboundOutput::Handshake(info) => {
                debug!(peer_id = %self.peer_id, overlay = %info.swarm_peer.overlay(), "Handshake completed (inbound)");
                self.state = State::Ready;
                self.pending_events
                    .push_back(Event::HandshakeCompleted(info));
            }
            TopologyInboundOutput::Hive(peers) => {
                debug!(peer_id = %self.peer_id, count = peers.len(), "Received hive peers");
                self.pending_events
                    .push_back(Event::HivePeersReceived(peers));
            }
            TopologyInboundOutput::Pingpong => {
                trace!(peer_id = %self.peer_id, "Responded to ping");
                self.pending_events.push_back(Event::PingpongPingReceived);
            }
        }
    }

    fn handle_outbound_output(
        &mut self,
        output: TopologyOutboundOutput,
        info: TopologyOutboundInfo,
    ) {
        match (output, info) {
            (
                TopologyOutboundOutput::Handshake(handshake_info),
                TopologyOutboundInfo::Handshake,
            ) => {
                self.handshake_outbound_pending = false;
                debug!(peer_id = %self.peer_id, overlay = %handshake_info.swarm_peer.overlay(), "Handshake completed (outbound)");
                self.state = State::Ready;
                self.pending_events
                    .push_back(Event::HandshakeCompleted(handshake_info));
            }
            (TopologyOutboundOutput::Hive, TopologyOutboundInfo::Hive) => {
                self.hive_outbound_pending = false;
                trace!(peer_id = %self.peer_id, "Hive broadcast completed");
                self.pending_events.push_back(Event::HiveBroadcastComplete);
            }
            (
                TopologyOutboundOutput::Pingpong(pong),
                TopologyOutboundInfo::Pingpong { sent_at },
            ) => {
                self.pingpong_outbound_pending = false;
                self.pending_ping = None;
                let rtt = sent_at.elapsed();
                trace!(peer_id = %self.peer_id, rtt_ms = rtt.as_millis(), response = %pong.response, "Pong received");
                self.pending_events.push_back(Event::PingpongPong { rtt });
            }
            _ => {
                warn!(peer_id = %self.peer_id, "Mismatched outbound output and info");
            }
        }
    }
}

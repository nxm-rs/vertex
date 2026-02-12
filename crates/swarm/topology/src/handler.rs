//! Per-connection handler for topology protocols.
//!
//! Manages handshake, hive, and pingpong on a single connection. The handler
//! starts in `Handshaking` state and transitions to `Ready` after successful
//! handshake, at which point hive and pingpong become available.
//!
//! Internal [`Command`] and [`Event`] types communicate between the handler
//! and [`TopologyBehaviour`](crate::TopologyBehaviour). External code should
//! subscribe to [`TopologyServiceEvent`](crate::TopologyServiceEvent) for peer
//! state changes.

use std::{
    collections::VecDeque,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use libp2p::{
    Multiaddr, PeerId,
    swarm::{
        StreamUpgradeError, SubstreamProtocol,
        handler::{
            ConnectionEvent, ConnectionHandler, ConnectionHandlerEvent, FullyNegotiatedInbound,
            FullyNegotiatedOutbound,
        },
    },
};
use tracing::{debug, info_span, trace, warn};
use vertex_net_handshake::{HANDSHAKE_TIMEOUT, HandshakeError, HandshakeInfo};
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_peer::SwarmPeer;

use crate::nat_discovery::NatDiscovery;

use crate::protocol::{
    TopologyInboundOutput, TopologyInboundUpgrade, TopologyOutboundInfo, TopologyOutboundOutput,
    TopologyOutboundUpgrade, TopologyUpgradeError,
};

/// Handler-specific configuration extracted from TopologyConfig.
#[derive(Debug, Clone)]
pub(crate) struct HandlerConfig {
    pub hive_timeout: Duration,
    pub pingpong_timeout: Duration,
    pub pingpong_greeting: String,
}

/// Commands from behaviour to handler (internal).
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

/// Events from handler to behaviour (internal).
pub enum Event {
    /// Handshake completed.
    HandshakeCompleted {
        info: Box<HandshakeInfo>,
        /// Duration from handler creation to handshake completion.
        handshake_duration: Duration,
    },
    /// Handshake failed.
    HandshakeFailed {
        error: HandshakeError,
        /// Duration from handler creation to failure.
        handshake_duration: Duration,
    },
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
            Self::HandshakeCompleted { info, handshake_duration } => f
                .debug_struct("HandshakeCompleted")
                .field("peer_id", &info.peer_id)
                .field("duration_ms", &handshake_duration.as_millis())
                .finish(),
            Self::HandshakeFailed { error, handshake_duration } => f
                .debug_struct("HandshakeFailed")
                .field("error", error)
                .field("duration_ms", &handshake_duration.as_millis())
                .finish(),
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

/// Per-connection handler for topology protocols.
pub struct TopologyHandler<I: SwarmIdentity> {
    config: HandlerConfig,
    identity: Arc<I>,
    peer_id: PeerId,
    remote_addr: Multiaddr,
    nat_discovery: Arc<NatDiscovery>,
    state: State,
    pending_events: VecDeque<Event>,
    pending_hive_outbound: VecDeque<Vec<SwarmPeer>>,
    should_initiate_handshake: bool,
    handshake_outbound_pending: bool,
    hive_outbound_pending: bool,
    pingpong_outbound_pending: bool,
    pending_ping_command: Option<String>,
    /// When this handler was created (for handshake duration tracking).
    created_at: Instant,
}

impl<I: SwarmIdentity> TopologyHandler<I> {
    /// Create a new handler.
    pub(crate) fn new(
        config: HandlerConfig,
        identity: Arc<I>,
        peer_id: PeerId,
        remote_addr: &Multiaddr,
        nat_discovery: Arc<NatDiscovery>,
    ) -> Self {
        Self {
            config,
            identity,
            peer_id,
            remote_addr: remote_addr.clone(),
            nat_discovery,
            state: State::Handshaking,
            pending_events: VecDeque::new(),
            pending_hive_outbound: VecDeque::new(),
            should_initiate_handshake: false,
            handshake_outbound_pending: false,
            hive_outbound_pending: false,
            pingpong_outbound_pending: false,
            pending_ping_command: None,
            created_at: Instant::now(),
        }
    }

    /// Returns the duration since this handler was created.
    pub fn elapsed(&self) -> Duration {
        self.created_at.elapsed()
    }

    fn inbound_upgrade(&self) -> TopologyInboundUpgrade<I> {
        TopologyInboundUpgrade::new(
            self.identity.clone(),
            self.peer_id,
            self.remote_addr.clone(),
            self.nat_discovery.clone(),
        )
    }

    fn is_ready(&self) -> bool {
        matches!(self.state, State::Ready)
    }
}

#[allow(deprecated)]
impl<I: SwarmIdentity> ConnectionHandler for TopologyHandler<I> {
    type FromBehaviour = Command;
    type ToBehaviour = Event;
    type InboundProtocol = TopologyInboundUpgrade<I>;
    type OutboundProtocol = TopologyOutboundUpgrade<I>;
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
            let upgrade = TopologyOutboundUpgrade::handshake(
                self.identity.clone(),
                self.peer_id,
                self.remote_addr.clone(),
                self.nat_discovery.clone(),
            );
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
                self.nat_discovery.clone(),
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
            let upgrade = TopologyOutboundUpgrade::pingpong(
                self.identity.clone(),
                self.peer_id,
                self.remote_addr.clone(),
                greeting,
                self.nat_discovery.clone(),
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
                        let handshake_error = extract_handshake_error(error.error);
                        self.state = State::Failed;
                        self.pending_events.push_back(Event::HandshakeFailed {
                            error: handshake_error,
                            handshake_duration: self.created_at.elapsed(),
                        });
                    }
                    TopologyOutboundInfo::Hive => {
                        self.hive_outbound_pending = false;
                        self.pending_events
                            .push_back(Event::HiveError(error.error.to_string()));
                    }
                    TopologyOutboundInfo::Pingpong { .. } => {
                        self.pingpong_outbound_pending = false;
                        self.pending_events
                            .push_back(Event::PingpongError(error.error.to_string()));
                    }
                }
            }

            ConnectionEvent::ListenUpgradeError(error) => {
                warn!(peer_id = %self.peer_id, error = %error.error, "Inbound upgrade error");
                if matches!(self.state, State::Handshaking) {
                    let handshake_error = extract_handshake_error_from_topology(error.error);
                    self.state = State::Failed;
                    self.pending_events.push_back(Event::HandshakeFailed {
                        error: handshake_error,
                        handshake_duration: self.created_at.elapsed(),
                    });
                }
            }

            _ => {}
        }
    }
}

impl<I: SwarmIdentity> TopologyHandler<I> {
    fn handle_inbound_output(&mut self, output: TopologyInboundOutput) {
        match output {
            TopologyInboundOutput::Handshake(info) => {
                let duration = self.created_at.elapsed();
                let _span = info_span!(
                    "handshake_complete",
                    peer_id = %self.peer_id,
                    overlay = %info.swarm_peer.overlay(),
                    direction = "inbound",
                    duration_ms = duration.as_millis() as u64,
                )
                .entered();
                debug!("Handshake completed");
                self.state = State::Ready;
                self.pending_events.push_back(Event::HandshakeCompleted {
                    info,
                    handshake_duration: duration,
                });
            }
            TopologyInboundOutput::Hive(peers) => {
                let _span = info_span!(
                    "hive_peers_received",
                    peer_id = %self.peer_id,
                    peer_count = peers.len(),
                )
                .entered();
                debug!("Received hive peers");
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
                let duration = self.created_at.elapsed();
                let _span = info_span!(
                    "handshake_complete",
                    peer_id = %self.peer_id,
                    overlay = %handshake_info.swarm_peer.overlay(),
                    direction = "outbound",
                    duration_ms = duration.as_millis() as u64,
                )
                .entered();
                debug!("Handshake completed");
                self.state = State::Ready;
                self.pending_events.push_back(Event::HandshakeCompleted {
                    info: handshake_info,
                    handshake_duration: duration,
                });
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
                let rtt = sent_at.elapsed();
                let _span = info_span!(
                    "pingpong",
                    peer_id = %self.peer_id,
                    rtt_ms = rtt.as_millis() as u64,
                )
                .entered();
                trace!(response = %pong.response, "Pong received");
                self.pending_events.push_back(Event::PingpongPong { rtt });
            }
            _ => {
                warn!(peer_id = %self.peer_id, "Mismatched outbound output and info");
            }
        }
    }
}

/// Extract HandshakeError from StreamUpgradeError (for dial/outbound errors).
fn extract_handshake_error(error: StreamUpgradeError<TopologyUpgradeError>) -> HandshakeError {
    match error {
        StreamUpgradeError::Timeout => HandshakeError::Timeout,
        StreamUpgradeError::Io(e) => HandshakeError::Io(e),
        StreamUpgradeError::Apply(e) => extract_handshake_error_from_topology(e),
        StreamUpgradeError::NegotiationFailed => {
            HandshakeError::UpgradeError("protocol negotiation failed".to_string())
        }
    }
}

/// Extract HandshakeError from TopologyUpgradeError (for listen/inbound errors).
fn extract_handshake_error_from_topology(error: TopologyUpgradeError) -> HandshakeError {
    match error {
        TopologyUpgradeError::Handshake(e) => e,
        other => HandshakeError::UpgradeError(other.to_string()),
    }
}

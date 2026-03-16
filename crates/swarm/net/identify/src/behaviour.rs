//! NetworkBehaviour implementation for identify with targeted push support.

use std::{
    collections::{hash_map::Entry, HashMap, HashSet, VecDeque},
    num::NonZeroUsize,
    sync::Arc,
    task::{Context, Poll},
};

use lru::LruCache;

use parking_lot::RwLock;

use libp2p::core::{
    multiaddr::{self, Protocol},
    transport::PortUse,
    ConnectedPoint, Endpoint, Multiaddr,
};
use libp2p::identity::{Keypair, PeerId, PublicKey};
use libp2p::swarm::{
    behaviour::{ConnectionClosed, ConnectionEstablished, DialFailure, FromSwarm},
    ConnectionDenied, ConnectionId, DialError, ExternalAddresses, ListenAddresses,
    NetworkBehaviour, NotifyHandler, PeerAddresses, StreamUpgradeError, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm, _address_translation,
};

use crate::{
    handler::{self, Handler, InEvent},
    metrics::{self, IdentifyErrorKind},
    protocol::{Info, UpgradeError},
    Config,
};

fn is_quic_addr(addr: &Multiaddr, v1: bool) -> bool {
    use Protocol::*;
    let mut iter = addr.iter();
    let Some(first) = iter.next() else {
        return false;
    };
    let Some(second) = iter.next() else {
        return false;
    };
    let Some(third) = iter.next() else {
        return false;
    };
    let fourth = iter.next();
    let fifth = iter.next();

    matches!(first, Ip4(_) | Ip6(_) | Dns(_) | Dns4(_) | Dns6(_))
        && matches!(second, Udp(_))
        && if v1 {
            matches!(third, QuicV1)
        } else {
            matches!(third, Quic)
        }
        && matches!(fourth, Some(P2p(_)) | None)
        && fifth.is_none()
}

fn is_tcp_addr(addr: &Multiaddr) -> bool {
    use Protocol::*;

    let mut iter = addr.iter();

    let Some(first) = iter.next() else {
        return false;
    };

    let Some(second) = iter.next() else {
        return false;
    };

    matches!(first, Ip4(_) | Ip6(_) | Dns(_) | Dns4(_) | Dns6(_)) && matches!(second, Tcp(_))
}

/// Maximum cached agent versions (bounds memory from peer churn).
const MAX_AGENT_VERSIONS: NonZeroUsize = match NonZeroUsize::new(1024) {
    Some(v) => v,
    None => unreachable!(),
};

/// Shared agent version map populated by identify exchanges.
pub type AgentVersions = Arc<RwLock<LruCache<PeerId, String>>>;

/// Create a new bounded agent version cache.
pub fn new_agent_versions() -> AgentVersions {
    Arc::new(RwLock::new(LruCache::new(MAX_AGENT_VERSIONS)))
}

/// Network behaviour for identify protocol with targeted push support.
pub struct Behaviour {
    config: Config,
    local_key: Arc<KeyType>,
    connected: HashMap<PeerId, HashMap<ConnectionId, Multiaddr>>,
    our_observed_addresses: HashMap<ConnectionId, Multiaddr>,
    outbound_connections_with_ephemeral_port: HashSet<ConnectionId>,
    events: VecDeque<ToSwarm<Event, InEvent>>,
    discovered_peers: PeerCache,
    listen_addresses: ListenAddresses,
    external_addresses: ExternalAddresses,
    /// Per-connection start times for measuring identify exchange duration.
    connection_timers: HashMap<ConnectionId, std::time::Instant>,
    /// Agent versions received via identify, shared with topology.
    agent_versions: AgentVersions,
}

/// Event emitted by the identify behaviour.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Event {
    /// Identification information received from a peer.
    Received {
        connection_id: ConnectionId,
        peer_id: PeerId,
        info: Info,
    },
    /// Identification information sent to a peer.
    Sent {
        connection_id: ConnectionId,
        peer_id: PeerId,
    },
    /// Identification information pushed to a peer.
    Pushed {
        connection_id: ConnectionId,
        peer_id: PeerId,
        info: Info,
    },
    /// Error during identification.
    Error {
        connection_id: ConnectionId,
        peer_id: PeerId,
        error: StreamUpgradeError<UpgradeError>,
    },
}

impl Event {
    pub fn connection_id(&self) -> ConnectionId {
        match self {
            Event::Received { connection_id, .. }
            | Event::Sent { connection_id, .. }
            | Event::Pushed { connection_id, .. }
            | Event::Error { connection_id, .. } => *connection_id,
        }
    }
}

impl Behaviour {
    /// Create a new identify behaviour with the given public key.
    pub fn new(config: Config, agent_versions: AgentVersions) -> Self {
        let discovered_peers = match NonZeroUsize::new(config.cache_size) {
            None => PeerCache::disabled(),
            Some(size) => PeerCache::enabled(size),
        };

        let local_key = Arc::new(KeyType::PublicKey(config.local_public_key.clone()));

        Self {
            config,
            local_key,
            connected: HashMap::new(),
            our_observed_addresses: Default::default(),
            outbound_connections_with_ephemeral_port: Default::default(),
            events: VecDeque::new(),
            discovered_peers,
            listen_addresses: Default::default(),
            external_addresses: Default::default(),
            connection_timers: HashMap::new(),
            agent_versions,
        }
    }

    /// Create a new identify behaviour with a keypair for signed peer records.
    pub fn new_with_keypair(config: Config, keypair: &Keypair, agent_versions: AgentVersions) -> Self {
        let discovered_peers = match NonZeroUsize::new(config.cache_size) {
            None => PeerCache::disabled(),
            Some(size) => PeerCache::enabled(size),
        };

        let local_key = Arc::new(KeyType::Keypair {
            keypair: keypair.clone(),
            public_key: keypair.public(),
        });

        Self {
            config,
            local_key,
            connected: HashMap::new(),
            our_observed_addresses: Default::default(),
            outbound_connections_with_ephemeral_port: Default::default(),
            events: VecDeque::new(),
            discovered_peers,
            listen_addresses: Default::default(),
            external_addresses: Default::default(),
            connection_timers: HashMap::new(),
            agent_versions,
        }
    }

    /// Push the local peer information to the given peers.
    pub fn push<I>(&mut self, peers: I)
    where
        I: IntoIterator<Item = PeerId>,
    {
        for p in peers {
            if !self.connected.contains_key(&p) {
                tracing::debug!(peer=%p, "Not pushing to peer because we are not connected");
                continue;
            }

            self.events.push_back(ToSwarm::NotifyHandler {
                peer_id: p,
                handler: NotifyHandler::Any,
                event: InEvent::Push,
            });
        }
    }

    /// Push specific addresses to a specific peer (targeted push).
    ///
    /// This enables sending custom addresses (e.g., observed addresses) to specific
    /// peers, bypassing the normal external address set. Useful for NAT traversal
    /// where a peer's observed address should be pushed back to them.
    pub fn push_with_addresses(&mut self, peer_id: PeerId, addresses: Vec<Multiaddr>) {
        if !self.connected.contains_key(&peer_id) {
            tracing::debug!(peer=%peer_id, "Not pushing to peer because we are not connected");
            return;
        }

        tracing::debug!(
            peer=%peer_id,
            ?addresses,
            "Pushing targeted addresses to peer"
        );

        self.events.push_back(ToSwarm::NotifyHandler {
            peer_id,
            handler: NotifyHandler::Any,
            event: InEvent::PushWithAddresses(addresses),
        });
    }

    fn on_connection_established(
        &mut self,
        ConnectionEstablished {
            peer_id,
            connection_id: conn,
            endpoint,
            failed_addresses,
            ..
        }: ConnectionEstablished,
    ) {
        let addr = match endpoint {
            ConnectedPoint::Dialer { address, .. } => address.clone(),
            ConnectedPoint::Listener { send_back_addr, .. } => send_back_addr.clone(),
        };

        self.connected
            .entry(peer_id)
            .or_default()
            .insert(conn, addr);

        self.connection_timers.insert(conn, std::time::Instant::now());

        if let Some(cache) = self.discovered_peers.0.as_mut() {
            for addr in failed_addresses {
                cache.remove(&peer_id, addr);
            }
        }
    }

    fn all_addresses(&self) -> HashSet<Multiaddr> {
        let mut addrs = HashSet::from_iter(self.external_addresses.iter().cloned());
        if !self.config.hide_listen_addrs {
            addrs.extend(self.listen_addresses.iter().cloned());
        };
        addrs
    }

    fn emit_new_external_addr_candidate_event(
        &mut self,
        connection_id: ConnectionId,
        observed: &Multiaddr,
    ) {
        if self
            .outbound_connections_with_ephemeral_port
            .contains(&connection_id)
        {
            let translated_addresses = {
                let mut addrs: Vec<_> = self
                    .listen_addresses
                    .iter()
                    .filter_map(|server| {
                        if (is_tcp_addr(server) && is_tcp_addr(observed))
                            || (is_quic_addr(server, true) && is_quic_addr(observed, true))
                            || (is_quic_addr(server, false) && is_quic_addr(observed, false))
                        {
                            _address_translation(server, observed)
                        } else {
                            None
                        }
                    })
                    .collect();

                addrs.sort_unstable();
                addrs.dedup();
                addrs
            };

            if translated_addresses.is_empty() {
                self.events
                    .push_back(ToSwarm::NewExternalAddrCandidate(observed.clone()));
            } else {
                for addr in translated_addresses {
                    self.events
                        .push_back(ToSwarm::NewExternalAddrCandidate(addr));
                }
            }
            return;
        }

        self.events
            .push_back(ToSwarm::NewExternalAddrCandidate(observed.clone()));
    }
}

impl NetworkBehaviour for Behaviour {
    type ConnectionHandler = Handler;
    type ToSwarm = Event;

    fn handle_established_inbound_connection(
        &mut self,
        _: ConnectionId,
        peer: PeerId,
        _: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(Handler::new(
            self.config.interval,
            peer,
            self.local_key.clone(),
            self.config.protocol_version.clone(),
            self.config.agent_version.clone(),
            remote_addr.clone(),
            self.all_addresses(),
        ))
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        _: Endpoint,
        port_use: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        let mut addr = addr.clone();
        if matches!(addr.iter().last(), Some(multiaddr::Protocol::P2p(_))) {
            addr.pop();
        }

        if port_use == PortUse::New {
            self.outbound_connections_with_ephemeral_port
                .insert(connection_id);
        }

        Ok(Handler::new(
            self.config.interval,
            peer,
            self.local_key.clone(),
            self.config.protocol_version.clone(),
            self.config.agent_version.clone(),
            addr.clone(),
            self.all_addresses(),
        ))
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        match event {
            handler::Event::Identified(mut info) => {
                info.listen_addrs
                    .retain(|addr| multiaddr_matches_peer_id(addr, &peer_id));

                // Store agent version for shared access by topology.
                self.agent_versions.write().put(peer_id, info.agent_version.clone());

                // Record metrics with the remote peer's agent version.
                let duration = self
                    .connection_timers
                    .remove(&connection_id)
                    .map(|t| t.elapsed())
                    .unwrap_or_default();
                metrics::record_received(self.config.purpose, &info.agent_version, duration);

                let observed = info.observed_addr.clone();
                self.events
                    .push_back(ToSwarm::GenerateEvent(Event::Received {
                        connection_id,
                        peer_id,
                        info: info.clone(),
                    }));

                if let Some(ref mut discovered_peers) = self.discovered_peers.0 {
                    for address in &info.listen_addrs {
                        if discovered_peers.add(peer_id, address.clone()) {
                            self.events.push_back(ToSwarm::NewExternalAddrOfPeer {
                                peer_id,
                                address: address.clone(),
                            });
                        }
                    }
                }

                match self.our_observed_addresses.entry(connection_id) {
                    Entry::Vacant(not_yet_observed) => {
                        not_yet_observed.insert(observed.clone());
                        self.emit_new_external_addr_candidate_event(connection_id, &observed);
                    }
                    Entry::Occupied(already_observed) if already_observed.get() == &observed => {}
                    Entry::Occupied(mut already_observed) => {
                        tracing::info!(
                            old_address=%already_observed.get(),
                            new_address=%observed,
                            "Our observed address on connection {connection_id} changed",
                        );

                        *already_observed.get_mut() = observed.clone();
                        self.emit_new_external_addr_candidate_event(connection_id, &observed);
                    }
                }
            }
            handler::Event::Identification => {
                metrics::record_sent(self.config.purpose);
                self.events.push_back(ToSwarm::GenerateEvent(Event::Sent {
                    connection_id,
                    peer_id,
                }));
            }
            handler::Event::IdentificationPushed(info) => {
                metrics::record_pushed(self.config.purpose);
                self.events.push_back(ToSwarm::GenerateEvent(Event::Pushed {
                    connection_id,
                    peer_id,
                    info,
                }));
            }
            handler::Event::IdentificationError(error) => {
                let duration = self
                    .connection_timers
                    .remove(&connection_id)
                    .map(|t| t.elapsed())
                    .unwrap_or_default();
                let kind = match &error {
                    StreamUpgradeError::Timeout => IdentifyErrorKind::Timeout,
                    _ => IdentifyErrorKind::Apply,
                };
                metrics::record_error(self.config.purpose, kind, duration);
                self.events.push_back(ToSwarm::GenerateEvent(Event::Error {
                    connection_id,
                    peer_id,
                    error,
                }));
            }
        }
    }

    #[tracing::instrument(level = "trace", name = "NetworkBehaviour::poll", skip(self))]
    fn poll(&mut self, _: &mut Context<'_>) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(event);
        }

        Poll::Pending
    }

    fn handle_pending_outbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        maybe_peer: Option<PeerId>,
        _addresses: &[Multiaddr],
        _effective_role: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        let Some(peer) = maybe_peer else {
            return Ok(vec![]);
        };

        Ok(self.discovered_peers.get(&peer))
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        let listen_addr_changed = self.listen_addresses.on_swarm_event(&event);
        let external_addr_changed = self.external_addresses.on_swarm_event(&event);

        if listen_addr_changed || external_addr_changed {
            let change_events = self
                .connected
                .iter()
                .flat_map(|(peer, map)| map.keys().map(|id| (*peer, id)))
                .map(|(peer_id, connection_id)| ToSwarm::NotifyHandler {
                    peer_id,
                    handler: NotifyHandler::One(*connection_id),
                    event: InEvent::AddressesChanged(self.all_addresses()),
                })
                .collect::<Vec<_>>();

            self.events.extend(change_events)
        }

        if listen_addr_changed && self.config.push_listen_addr_updates {
            let push_events = self.connected.keys().map(|peer| ToSwarm::NotifyHandler {
                peer_id: *peer,
                handler: NotifyHandler::Any,
                event: InEvent::Push,
            });

            self.events.extend(push_events);
        }

        match event {
            FromSwarm::ConnectionEstablished(connection_established) => {
                self.on_connection_established(connection_established)
            }
            FromSwarm::ConnectionClosed(ConnectionClosed {
                peer_id,
                connection_id,
                remaining_established,
                ..
            }) => {
                if remaining_established == 0 {
                    self.connected.remove(&peer_id);
                    self.agent_versions.write().pop(&peer_id);
                } else if let Some(addrs) = self.connected.get_mut(&peer_id) {
                    addrs.remove(&connection_id);
                }

                self.our_observed_addresses.remove(&connection_id);
                self.connection_timers.remove(&connection_id);
                self.outbound_connections_with_ephemeral_port
                    .remove(&connection_id);
            }
            FromSwarm::DialFailure(DialFailure {
                peer_id: Some(peer_id),
                error,
                ..
            }) => {
                if let Some(cache) = self.discovered_peers.0.as_mut() {
                    match error {
                        DialError::Transport(errors) => {
                            for (addr, _error) in errors {
                                cache.remove(&peer_id, addr);
                            }
                        }
                        DialError::WrongPeerId { address, .. }
                        | DialError::LocalPeerId { address } => {
                            cache.remove(&peer_id, address);
                        }
                        _ => (),
                    };
                }
            }
            _ => {}
        }
    }
}

fn multiaddr_matches_peer_id(addr: &Multiaddr, peer_id: &PeerId) -> bool {
    let last_component = addr.iter().last();
    if let Some(multiaddr::Protocol::P2p(multi_addr_peer_id)) = last_component {
        return multi_addr_peer_id == *peer_id;
    }
    true
}

struct PeerCache(Option<PeerAddresses>);

impl PeerCache {
    fn disabled() -> Self {
        Self(None)
    }

    fn enabled(size: NonZeroUsize) -> Self {
        Self(Some(PeerAddresses::new(size)))
    }

    fn get(&mut self, peer: &PeerId) -> Vec<Multiaddr> {
        if let Some(cache) = self.0.as_mut() {
            cache.get(peer).collect()
        } else {
            Vec::new()
        }
    }
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum KeyType {
    PublicKey(PublicKey),
    Keypair {
        keypair: Keypair,
        public_key: PublicKey,
    },
}

impl From<PublicKey> for KeyType {
    fn from(value: PublicKey) -> Self {
        Self::PublicKey(value)
    }
}

impl From<&Keypair> for KeyType {
    fn from(value: &Keypair) -> Self {
        Self::Keypair {
            public_key: value.public(),
            keypair: value.clone(),
        }
    }
}

impl KeyType {
    pub(crate) fn public_key(&self) -> &PublicKey {
        match &self {
            KeyType::PublicKey(pubkey) => pubkey,
            KeyType::Keypair { public_key, .. } => public_key,
        }
    }
}

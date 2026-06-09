//! NetworkBehaviour for handshake protocol.

use std::{
    collections::VecDeque,
    sync::Arc,
    task::{Context, Poll},
};

use libp2p::{
    Multiaddr, PeerId,
    swarm::{
        ConnectionClosed, ConnectionId, FromSwarm, NetworkBehaviour, NotifyHandler, THandler,
        THandlerInEvent, THandlerOutEvent, ToSwarm,
    },
};
use parking_lot::RwLock;
use tracing::debug;
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_peer::{SwarmPeer, Timestamp};
use vertex_swarm_spec::SwarmSpec;

use vertex_net_peer_registry::ConnectionDirection;

use crate::{
    AddressProvider, HandshakeError, HandshakeInfo, SharedAdmissionControl,
    admission::default_admission_control,
    cache::{CachedSelfRecord, SELF_RECORD_REFRESH_INTERVAL, fingerprint, needs_resign},
    handler::{HandshakeCommand, HandshakeConfig, HandshakeHandler, HandshakeHandlerEvent},
};

/// Events emitted by HandshakeBehaviour.
#[derive(Debug)]
pub enum HandshakeEvent {
    /// Handshake completed successfully.
    Completed {
        peer_id: PeerId,
        connection_id: ConnectionId,
        direction: ConnectionDirection,
        info: Box<HandshakeInfo>,
    },
    /// Handshake failed.
    Failed {
        peer_id: PeerId,
        connection_id: ConnectionId,
        direction: ConnectionDirection,
        error: HandshakeError,
    },
}

/// Behaviour for the Swarm handshake protocol.
pub struct HandshakeBehaviour<I, A> {
    config: Arc<HandshakeConfig>,
    identity: Arc<I>,
    address_provider: Arc<A>,
    /// Admission gate consulted before each side commits to its final
    /// message; defaults to [`AlwaysAccept`](crate::AlwaysAccept).
    admission_control: SharedAdmissionControl,
    events: VecDeque<ToSwarm<HandshakeEvent, HandshakeCommand>>,
    /// Track direction per connection for event attribution.
    connection_directions: std::collections::HashMap<ConnectionId, ConnectionDirection>,
    /// Self record signed once per address-set change (plus a periodic
    /// refresh), reused byte-identically across handshakes with an unchanged
    /// advertised set. See [`crate::cache`].
    cached_record: RwLock<Option<CachedSelfRecord>>,
}

impl<I, A> HandshakeBehaviour<I, A>
where
    I: SwarmIdentity + 'static,
    A: AddressProvider + 'static,
{
    /// Create a new handshake behaviour with the given purpose label for metrics.
    pub fn new(identity: Arc<I>, address_provider: Arc<A>, purpose: &'static str) -> Self {
        Self {
            config: Arc::new(HandshakeConfig::new(purpose)),
            identity,
            address_provider,
            admission_control: default_admission_control(),
            events: VecDeque::new(),
            connection_directions: std::collections::HashMap::new(),
            cached_record: RwLock::new(None),
        }
    }

    /// Return the signed self record to advertise to a peer reached at
    /// `remote_addr`, reusing the cache when the advertised address set is
    /// unchanged and fresh.
    ///
    /// Resolves the scope-filtered, ordered advertised set for the peer. An
    /// empty set yields `None`: the protocol then signs a last-resort record
    /// over just the peer-observed address during the exchange. A non-empty set
    /// is fingerprinted; if the fingerprint matches a cached record still inside
    /// [`SELF_RECORD_REFRESH_INTERVAL`] the cached record is cloned (same
    /// timestamp, same signature), otherwise the record is re-signed with a
    /// current timestamp and cached. Concurrent misses single-flight under the
    /// write lock.
    fn cached_self_record(&self, remote_addr: &Multiaddr) -> Option<SwarmPeer> {
        let addrs = self.address_provider.addresses_for_peer(remote_addr);
        if addrs.is_empty() {
            return None;
        }

        let fp = fingerprint(&addrs);
        let now = Timestamp::now();

        // Fast path: a fresh cache hit needs only a read lock.
        if let Some(cached) = self.cached_record.read().as_ref()
            && !needs_resign(Some(cached), fp, now, SELF_RECORD_REFRESH_INTERVAL)
        {
            return Some(cached.record.clone());
        }

        // Slow path: re-sign under the write lock, double-checking so a
        // concurrent miss that already signed is not duplicated.
        let mut guard = self.cached_record.write();
        if !needs_resign(guard.as_ref(), fp, now, SELF_RECORD_REFRESH_INTERVAL) {
            // Safe: `needs_resign` returns false only when the cache is present.
            if let Some(cached) = guard.as_ref() {
                return Some(cached.record.clone());
            }
        }

        match self.sign_self_record(addrs, now) {
            Ok(record) => {
                *guard = Some(CachedSelfRecord {
                    fingerprint: fp,
                    signed_at: now,
                    record: record.clone(),
                });
                Some(record)
            }
            Err(error) => {
                // A signing failure here is non-fatal: fall back to `None` so
                // the protocol attempts the last-resort observed-address sign.
                debug!(
                    ?error,
                    "self record signing failed; deferring to last-resort sign"
                );
                None
            }
        }
    }

    /// Sign a self record over `addrs` at `now`.
    fn sign_self_record(
        &self,
        addrs: Vec<Multiaddr>,
        now: Timestamp,
    ) -> Result<SwarmPeer, HandshakeError> {
        let signer = self.identity.signer();
        SwarmPeer::sign(
            &*signer,
            addrs,
            self.identity.overlay_address(),
            self.identity.spec().network_id(),
            self.identity.nonce(),
            now,
            None,
        )
        .map_err(HandshakeError::from)
    }

    /// Create with custom config.
    pub fn with_config(mut self, config: HandshakeConfig) -> Self {
        self.config = Arc::new(config);
        self
    }

    /// Install an admission control gate, replacing any previously
    /// installed gate (the default is [`AlwaysAccept`](crate::AlwaysAccept)).
    ///
    /// Consulted once per handshake just before the local side commits
    /// to its final message. On
    /// [`AdmissionDecision::Reject`](crate::AdmissionDecision::Reject)
    /// the handshake terminates with
    /// [`HandshakeError::AdmissionRejected`](crate::HandshakeError::AdmissionRejected).
    pub fn with_admission_control(mut self, admission_control: SharedAdmissionControl) -> Self {
        self.admission_control = admission_control;
        self
    }

    /// Initiate handshake on a connection with a resolved address.
    pub fn initiate(&mut self, peer_id: PeerId, connection_id: ConnectionId, addr: Multiaddr) {
        self.events.push_back(ToSwarm::NotifyHandler {
            peer_id,
            handler: NotifyHandler::One(connection_id),
            event: HandshakeCommand::Initiate(addr),
        });
    }
}

impl<I, A> NetworkBehaviour for HandshakeBehaviour<I, A>
where
    I: SwarmIdentity + 'static,
    A: AddressProvider + 'static,
{
    type ConnectionHandler = HandshakeHandler<I, A>;
    type ToSwarm = HandshakeEvent;

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        _local_addr: &Multiaddr,
        remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, libp2p::swarm::ConnectionDenied> {
        debug!(%peer, ?connection_id, %remote_addr, "Creating inbound handshake handler");
        self.connection_directions
            .insert(connection_id, ConnectionDirection::Inbound);
        let self_record = self.cached_self_record(remote_addr);
        Ok(HandshakeHandler::new_inbound(
            self.config.clone(),
            self.identity.clone(),
            peer,
            remote_addr.clone(),
            self.address_provider.clone(),
            self.admission_control.clone(),
            self_record,
        ))
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        _role_override: libp2p::core::Endpoint,
        _port_use: libp2p::core::transport::PortUse,
    ) -> Result<THandler<Self>, libp2p::swarm::ConnectionDenied> {
        debug!(%peer, ?connection_id, %addr, "Creating outbound handshake handler");
        self.connection_directions
            .insert(connection_id, ConnectionDirection::Outbound);
        let self_record = self.cached_self_record(addr);
        Ok(HandshakeHandler::new_outbound(
            self.config.clone(),
            self.identity.clone(),
            peer,
            addr.clone(),
            self.address_provider.clone(),
            self.admission_control.clone(),
            self_record,
        ))
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        if let FromSwarm::ConnectionClosed(ConnectionClosed {
            connection_id,
            peer_id,
            ..
        }) = event
        {
            self.connection_directions.remove(&connection_id);
            debug!(%peer_id, "Connection closed");
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        let direction = self
            .connection_directions
            .get(&connection_id)
            .copied()
            .unwrap_or(ConnectionDirection::Inbound);

        match event {
            HandshakeHandlerEvent::Completed { info } => {
                debug!(%peer_id, ?connection_id, ?direction, "Handshake completed");
                self.events
                    .push_back(ToSwarm::GenerateEvent(HandshakeEvent::Completed {
                        peer_id,
                        connection_id,
                        direction,
                        info,
                    }));
            }
            HandshakeHandlerEvent::Failed { error } => {
                debug!(%peer_id, ?connection_id, ?direction, ?error, "Handshake failed");
                self.events
                    .push_back(ToSwarm::GenerateEvent(HandshakeEvent::Failed {
                        peer_id,
                        connection_id,
                        direction,
                        error,
                    }));
            }
        }
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        if let Some(event) = self.events.pop_front() {
            return Poll::Ready(event);
        }
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use vertex_swarm_test_utils::test_identity_arc;

    /// Address provider returning a fixed advertised set regardless of peer.
    struct StubAddresses {
        addrs: Vec<Multiaddr>,
    }

    impl AddressProvider for StubAddresses {
        fn addresses_for_peer(&self, _peer_addr: &Multiaddr) -> Vec<Multiaddr> {
            self.addrs.clone()
        }

        fn local_peer_id(&self) -> Option<&PeerId> {
            None
        }
    }

    fn addr(s: &str) -> Multiaddr {
        s.parse().expect("valid multiaddr")
    }

    fn behaviour(addrs: Vec<Multiaddr>) -> HandshakeBehaviour<impl SwarmIdentity, StubAddresses> {
        HandshakeBehaviour::new(
            test_identity_arc(),
            Arc::new(StubAddresses { addrs }),
            "test",
        )
    }

    #[test]
    fn cached_record_is_byte_identical_across_calls() {
        // The core property: with an unchanged advertised set, two consecutive
        // signs return the same record (same timestamp, same signature), so a
        // receiver sees no delta to store and re-gossip.
        let remote = addr("/ip4/198.51.100.4/tcp/1634");
        let behaviour = behaviour(vec![addr("/ip4/8.8.4.4/tcp/1634")]);

        let first = behaviour
            .cached_self_record(&remote)
            .expect("non-empty set signs a record");
        let second = behaviour
            .cached_self_record(&remote)
            .expect("cached record is reused");

        assert_eq!(
            first.timestamp(),
            second.timestamp(),
            "cached record keeps a stable timestamp"
        );
        assert_eq!(
            first.signature(),
            second.signature(),
            "cached record keeps a byte-identical signature"
        );
        assert_eq!(first, second, "cached record is byte-identical");
    }

    #[test]
    fn changing_address_set_resigns() {
        // A different advertised set produces a different fingerprint, so the
        // record is re-signed (different multiaddrs, and a fresh signature).
        let remote = addr("/ip4/198.51.100.4/tcp/1634");
        let behaviour = behaviour(vec![addr("/ip4/8.8.4.4/tcp/1634")]);
        let first = behaviour
            .cached_self_record(&remote)
            .expect("non-empty set signs a record");

        // Swap the advertised set under the same behaviour by rebuilding it; the
        // cache is keyed by fingerprint, so a new set re-signs.
        let behaviour2 = HandshakeBehaviour::new(
            test_identity_arc(),
            Arc::new(StubAddresses {
                addrs: vec![addr("/ip4/1.1.1.1/tcp/1634")],
            }),
            "test",
        );
        let other = behaviour2
            .cached_self_record(&remote)
            .expect("non-empty set signs a record");

        assert_ne!(
            first.multiaddrs(),
            other.multiaddrs(),
            "a different advertised set yields a different record"
        );
    }

    #[test]
    fn empty_address_set_yields_no_cached_record() {
        // An empty advertised set defers to the protocol's last-resort sign.
        let remote = addr("/ip4/198.51.100.4/tcp/1634");
        let behaviour = behaviour(Vec::new());
        assert!(behaviour.cached_self_record(&remote).is_none());
    }
}

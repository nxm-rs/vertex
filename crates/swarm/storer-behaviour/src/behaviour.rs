//! `NetworkBehaviour` for pullsync: an inbound syncer backed by a
//! [`PullStorage`] snapshot, plus a puller command surface that opens outbound
//! cursor and range substreams.

use std::{
    collections::VecDeque,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use libp2p::{
    Multiaddr, PeerId,
    swarm::{
        ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, NotifyHandler, THandler,
        THandlerInEvent, THandlerOutEvent, ToSwarm,
    },
};
use strum::IntoStaticStr;
use vertex_net_ratelimiter::{KeyedRateLimiter, Quota};
use vertex_swarm_api::{Bin, PullStorage, StampedChunk};

use crate::error::PullsyncFailure;
use crate::handler::{PullsyncCommand, PullsyncHandler, PullsyncHandlerEvent};

/// Chunks served per second per peer, enforced by the per-connection handler
/// through a shared [`KeyedRateLimiter`].
const CHUNK_QUOTA: Quota = Quota::n_every(
    match std::num::NonZeroU32::new(vertex_swarm_net_pullsync::MAX_CHUNKS_PER_SECOND as u32) {
        Some(v) => v,
        None => unreachable!(),
    },
    Duration::from_secs(1),
);

/// Events emitted by [`PullsyncBehaviour`]. `request_id` echoes the command
/// that opened the exchange so the puller drops a stale buffered reply rather
/// than matching it to a later command for the same peer and bin; it never
/// crosses the wire.
#[derive(Debug, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum PullsyncEvent {
    /// A peer answered a cursor handshake with its per-bin cursors and reserve
    /// epoch.
    CursorsReceived {
        peer: PeerId,
        request_id: u64,
        cursors: Vec<u64>,
        epoch: u64,
    },
    /// A peer delivered a range page for `bin`. `topmost` is the highest id the
    /// offer covered; the puller advances its cursor to it.
    RangeDelivered {
        peer: PeerId,
        request_id: u64,
        bin: Bin,
        topmost: u64,
        chunks: Vec<StampedChunk>,
    },
    /// An outbound command against `peer` failed.
    Failed {
        peer: PeerId,
        request_id: u64,
        failure: PullsyncFailure,
    },
}

/// Pullsync behaviour: syncer (inbound) and puller command surface (outbound).
pub struct PullsyncBehaviour {
    /// Server snapshot the inbound responders read, injected as a trait object.
    storage: Arc<dyn PullStorage>,
    /// Shared into each handler so the per-peer chunks-per-second bucket
    /// survives reconnects; freed on the final `ConnectionClosed`.
    chunk_limit: Arc<KeyedRateLimiter<PeerId>>,
    events: VecDeque<ToSwarm<PullsyncEvent, PullsyncCommand>>,
}

impl PullsyncBehaviour {
    /// Construct with the storage snapshot the inbound syncer reads.
    pub fn new(storage: Arc<dyn PullStorage>) -> Self {
        Self {
            storage,
            chunk_limit: Arc::new(KeyedRateLimiter::new(CHUNK_QUOTA)),
            events: VecDeque::new(),
        }
    }

    /// Open the cursor handshake against `peer`. The peer's cursors arrive as a
    /// [`PullsyncEvent::CursorsReceived`] carrying `request_id`.
    pub fn fetch_cursors(&mut self, peer: PeerId, request_id: u64) {
        self.events.push_back(ToSwarm::NotifyHandler {
            peer_id: peer,
            handler: NotifyHandler::Any,
            event: PullsyncCommand::FetchCursors { request_id },
        });
    }

    /// Open a range exchange against `peer` for `bin` from `start`. The selected
    /// chunks arrive as a [`PullsyncEvent::RangeDelivered`] carrying `request_id`.
    pub fn sync_range(&mut self, peer: PeerId, request_id: u64, bin: Bin, start: u64) {
        self.events.push_back(ToSwarm::NotifyHandler {
            peer_id: peer,
            handler: NotifyHandler::Any,
            event: PullsyncCommand::SyncRange {
                request_id,
                bin,
                start,
            },
        });
    }

    fn make_handler(&self, peer: PeerId) -> PullsyncHandler {
        PullsyncHandler::new(
            peer,
            Arc::clone(&self.storage),
            Arc::clone(&self.chunk_limit),
        )
    }
}

impl NetworkBehaviour for PullsyncBehaviour {
    type ConnectionHandler = PullsyncHandler;
    type ToSwarm = PullsyncEvent;

    fn handle_established_inbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        peer: PeerId,
        _local_addr: &Multiaddr,
        _remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(self.make_handler(peer))
    }

    fn handle_established_outbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        peer: PeerId,
        _addr: &Multiaddr,
        _role_override: libp2p::core::Endpoint,
        _port_use: libp2p::core::transport::PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(self.make_handler(peer))
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        if let FromSwarm::ConnectionClosed(closed) = event
            && closed.remaining_established == 0
        {
            // Free the per-peer rate-limit bucket only when the last connection
            // closes; an earlier clear would let a peer reset its bucket by
            // churning a single connection.
            self.chunk_limit.clear(&closed.peer_id);
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        _connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        let event = match event {
            PullsyncHandlerEvent::CursorsReceived {
                request_id,
                cursors,
                epoch,
            } => PullsyncEvent::CursorsReceived {
                peer: peer_id,
                request_id,
                cursors,
                epoch,
            },
            PullsyncHandlerEvent::RangeDelivered {
                request_id,
                bin,
                topmost,
                chunks,
            } => PullsyncEvent::RangeDelivered {
                peer: peer_id,
                request_id,
                bin,
                topmost,
                chunks,
            },
            PullsyncHandlerEvent::OutboundFailed {
                request_id,
                failure,
            } => PullsyncEvent::Failed {
                peer: peer_id,
                request_id,
                failure,
            },
        };
        self.events.push_back(ToSwarm::GenerateEvent(event));
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

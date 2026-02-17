//! Connection state for peers in the registry.

use std::time::Instant;

use libp2p::{Multiaddr, PeerId, swarm::ConnectionId};

use crate::direction::ConnectionDirection;

/// Connection state for a peer in the registry.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ConnectionState<Id, R = ()> {
    /// Actively dialing (transport connection in progress).
    Dialing {
        peer_id: PeerId,
        /// Peer ID if known (None for bootnodes).
        id: Option<Id>,
        /// All addresses passed to libp2p (for diagnostics).
        addrs: Vec<Multiaddr>,
        started_at: Instant,
        reason: R,
    },
    /// Transport connected, handshake in progress.
    Handshaking {
        peer_id: PeerId,
        connection_id: ConnectionId,
        /// Peer ID if known from dial tracking.
        id: Option<Id>,
        direction: ConnectionDirection,
        started_at: Instant,
        reason: R,
    },
    /// Fully connected and handshake completed.
    Active {
        peer_id: PeerId,
        id: Id,
        connection_id: ConnectionId,
        connected_at: Instant,
        reason: R,
    },
}

impl<Id: Clone, R> ConnectionState<Id, R> {
    pub fn reason(&self) -> &R {
        match self {
            Self::Dialing { reason, .. } => reason,
            Self::Handshaking { reason, .. } => reason,
            Self::Active { reason, .. } => reason,
        }
    }

    pub fn direction(&self) -> Option<ConnectionDirection> {
        match self {
            Self::Handshaking { direction, .. } => Some(*direction),
            _ => None,
        }
    }

    pub fn id(&self) -> Option<Id> {
        match self {
            Self::Dialing { id, .. } => id.clone(),
            Self::Handshaking { id, .. } => id.clone(),
            Self::Active { id, .. } => Some(id.clone()),
        }
    }

    /// All addresses passed to libp2p for this dial (for diagnostics).
    pub fn addrs(&self) -> Option<&Vec<Multiaddr>> {
        match self {
            Self::Dialing { addrs, .. } => Some(addrs),
            _ => None,
        }
    }

    pub fn started_at(&self) -> Option<Instant> {
        match self {
            Self::Dialing { started_at, .. } => Some(*started_at),
            Self::Handshaking { started_at, .. } => Some(*started_at),
            _ => None,
        }
    }

    pub fn peer_id(&self) -> PeerId {
        match self {
            Self::Dialing { peer_id, .. } => *peer_id,
            Self::Handshaking { peer_id, .. } => *peer_id,
            Self::Active { peer_id, .. } => *peer_id,
        }
    }

    pub fn connection_id(&self) -> Option<ConnectionId> {
        match self {
            Self::Dialing { .. } => None,
            Self::Handshaking { connection_id, .. } => Some(*connection_id),
            Self::Active { connection_id, .. } => Some(*connection_id),
        }
    }

    pub fn connected_at(&self) -> Option<Instant> {
        match self {
            Self::Active { connected_at, .. } => Some(*connected_at),
            _ => None,
        }
    }

    pub fn is_dialing(&self) -> bool {
        matches!(self, Self::Dialing { .. })
    }

    pub fn is_handshaking(&self) -> bool {
        matches!(self, Self::Handshaking { .. })
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active { .. })
    }

    pub fn is_pending(&self) -> bool {
        self.is_dialing() || self.is_handshaking()
    }
}

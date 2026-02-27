//! Connection state for peers in the registry.

use std::time::Instant;

use libp2p::{PeerId, swarm::ConnectionId};

use crate::direction::ConnectionDirection;

/// Connection state for a peer in the registry.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ConnectionState<Id, R = ()> {
    /// Transport connected, awaiting application-level identity.
    Connected {
        peer_id: PeerId,
        connection_id: ConnectionId,
        /// Application-level ID if known from dial tracking.
        id: Option<Id>,
        direction: ConnectionDirection,
        started_at: Instant,
        reason: R,
    },
    /// Fully active with confirmed application-level identity.
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
            Self::Connected { reason, .. } => reason,
            Self::Active { reason, .. } => reason,
        }
    }

    pub fn direction(&self) -> Option<ConnectionDirection> {
        match self {
            Self::Connected { direction, .. } => Some(*direction),
            Self::Active { .. } => None,
        }
    }

    pub fn id(&self) -> Option<Id> {
        match self {
            Self::Connected { id, .. } => id.clone(),
            Self::Active { id, .. } => Some(id.clone()),
        }
    }

    pub fn started_at(&self) -> Option<Instant> {
        match self {
            Self::Connected { started_at, .. } => Some(*started_at),
            Self::Active { .. } => None,
        }
    }

    pub fn peer_id(&self) -> PeerId {
        match self {
            Self::Connected { peer_id, .. } => *peer_id,
            Self::Active { peer_id, .. } => *peer_id,
        }
    }

    pub fn connection_id(&self) -> Option<ConnectionId> {
        match self {
            Self::Connected { connection_id, .. } => Some(*connection_id),
            Self::Active { connection_id, .. } => Some(*connection_id),
        }
    }

    pub fn connected_at(&self) -> Option<Instant> {
        match self {
            Self::Active { connected_at, .. } => Some(*connected_at),
            Self::Connected { .. } => None,
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active { .. })
    }

    /// Whether this connection is pending (awaiting handshake completion).
    pub fn is_pending(&self) -> bool {
        self.is_connected()
    }
}

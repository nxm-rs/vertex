//! Generic peer connection lifecycle tracking.

use libp2p::swarm::ConnectionId;

/// Direction of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::Display, strum::IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
pub enum ConnectionDirection {
    /// We initiated the connection (dialed the peer).
    Outbound,
    /// The peer initiated the connection (they dialed us).
    Inbound,
}

impl ConnectionDirection {
    pub fn is_outbound(&self) -> bool {
        matches!(self, Self::Outbound)
    }

    pub fn is_inbound(&self) -> bool {
        matches!(self, Self::Inbound)
    }
}

/// Result of activating a connection (transitioning to Active state).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivateResult<Id> {
    /// New peer accepted.
    Accepted,
    /// Replaced existing connection - caller must close old connection.
    Replaced {
        old_peer_id: libp2p::PeerId,
        old_connection_id: ConnectionId,
        /// Some if the peer changed their ID (neighborhood migration).
        old_id: Option<Id>,
    },
}

impl<Id> ActivateResult<Id> {
    /// Returns true if a connection was replaced.
    pub fn is_replaced(&self) -> bool {
        matches!(self, Self::Replaced { .. })
    }

    /// Returns true if a new connection was accepted.
    pub fn is_accepted(&self) -> bool {
        matches!(self, Self::Accepted)
    }

    /// Get the old connection ID if a connection was replaced.
    pub fn old_connection_id(&self) -> Option<ConnectionId> {
        match self {
            Self::Replaced {
                old_connection_id, ..
            } => Some(*old_connection_id),
            Self::Accepted => None,
        }
    }
}

mod registry;
mod state;

pub use registry::PeerRegistry;
pub use state::ConnectionState;

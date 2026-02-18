//! Connection direction (inbound vs outbound).

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

//! Connection direction (inbound vs outbound).

/// Direction of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Outbound => "outbound",
            Self::Inbound => "inbound",
        }
    }
}

impl std::fmt::Display for ConnectionDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

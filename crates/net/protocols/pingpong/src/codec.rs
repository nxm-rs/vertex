//! Codec for pingpong protocol messages.

use vertex_net_codec::ProtocolCodec;

/// Codec for ping messages.
pub type PingCodec = ProtocolCodec<crate::proto::pingpong::Ping, Ping, PingpongCodecError>;

/// Codec for pong messages.
pub type PongCodec = ProtocolCodec<crate::proto::pingpong::Pong, Pong, PingpongCodecError>;

/// Error type for pingpong codec operations.
#[derive(Debug, thiserror::Error)]
pub enum PingpongCodecError {
    /// Protocol-level error (invalid message format, etc.)
    #[error("Protocol error: {0}")]
    Protocol(String),
    /// IO error during read/write
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<quick_protobuf_codec::Error> for PingpongCodecError {
    fn from(error: quick_protobuf_codec::Error) -> Self {
        PingpongCodecError::Protocol(error.to_string())
    }
}

/// A ping message with a greeting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ping {
    /// The greeting message.
    pub greeting: String,
}

impl Ping {
    /// Create a new ping with the given greeting.
    pub fn new(greeting: impl Into<String>) -> Self {
        Self {
            greeting: greeting.into(),
        }
    }
}

impl TryFrom<crate::proto::pingpong::Ping> for Ping {
    type Error = PingpongCodecError;

    fn try_from(value: crate::proto::pingpong::Ping) -> Result<Self, Self::Error> {
        Ok(Self {
            greeting: value.greeting,
        })
    }
}

impl From<Ping> for crate::proto::pingpong::Ping {
    fn from(value: Ping) -> Self {
        crate::proto::pingpong::Ping {
            greeting: value.greeting,
        }
    }
}

/// A pong response message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pong {
    /// The response message (typically the greeting wrapped in braces).
    pub response: String,
}

impl Pong {
    /// Create a new pong with the given response.
    pub fn new(response: impl Into<String>) -> Self {
        Self {
            response: response.into(),
        }
    }

    /// Create a pong response from a ping greeting (wraps in braces like Bee).
    pub fn from_greeting(greeting: &str) -> Self {
        Self {
            response: format!("{{{}}}", greeting),
        }
    }
}

impl TryFrom<crate::proto::pingpong::Pong> for Pong {
    type Error = PingpongCodecError;

    fn try_from(value: crate::proto::pingpong::Pong) -> Result<Self, Self::Error> {
        Ok(Self {
            response: value.response,
        })
    }
}

impl From<Pong> for crate::proto::pingpong::Pong {
    fn from(value: Pong) -> Self {
        crate::proto::pingpong::Pong {
            response: value.response,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ping_roundtrip() {
        let original = Ping::new("hello");
        let proto: crate::proto::pingpong::Ping = original.clone().into();
        let decoded = Ping::try_from(proto).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_pong_roundtrip() {
        let original = Pong::new("{hello}");
        let proto: crate::proto::pingpong::Pong = original.clone().into();
        let decoded = Pong::try_from(proto).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_pong_from_greeting() {
        let pong = Pong::from_greeting("ping");
        assert_eq!(pong.response, "{ping}");
    }
}

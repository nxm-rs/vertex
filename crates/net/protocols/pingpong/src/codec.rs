//! Codec for pingpong protocol messages.

use vertex_net_codec::{Codec, ProtoMessage, ProtocolCodecError};

/// Error type for pingpong codec operations.
///
/// Pingpong has no domain-specific errors, so we use the base `ProtocolCodecError`.
pub type PingpongCodecError = ProtocolCodecError;

/// Codec for ping messages.
pub type PingCodec = Codec<Ping, PingpongCodecError>;

/// Codec for pong messages.
pub type PongCodec = Codec<Pong, PingpongCodecError>;

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

impl ProtoMessage for Ping {
    type Proto = crate::proto::pingpong::Ping;
    type DecodeError = PingpongCodecError;

    fn into_proto(self) -> Self::Proto {
        crate::proto::pingpong::Ping {
            greeting: self.greeting,
        }
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        Ok(Self {
            greeting: proto.greeting,
        })
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

impl ProtoMessage for Pong {
    type Proto = crate::proto::pingpong::Pong;
    type DecodeError = PingpongCodecError;

    fn into_proto(self) -> Self::Proto {
        crate::proto::pingpong::Pong {
            response: self.response,
        }
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, Self::DecodeError> {
        Ok(Self {
            response: proto.response,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ping_roundtrip() {
        let original = Ping::new("hello");
        let proto = original.clone().into_proto();
        let decoded = Ping::from_proto(proto).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_pong_roundtrip() {
        let original = Pong::new("{hello}");
        let proto = original.clone().into_proto();
        let decoded = Pong::from_proto(proto).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn test_pong_from_greeting() {
        let pong = Pong::from_greeting("ping");
        assert_eq!(pong.response, "{ping}");
    }
}

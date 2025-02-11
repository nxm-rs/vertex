mod behaviour;
mod error;
mod handler;
mod protocol;

pub use behaviour::{Behaviour, Config, Event};
pub use error::HandshakeError;
use libp2p::StreamProtocol;
pub use protocol::ProtocolConfig;

pub(crate) const DEFAULT_PROTOCOL_NAME: StreamProtocol =
    StreamProtocol::new("/swarm/handshake/13.0.0");

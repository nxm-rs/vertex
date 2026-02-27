//! Generic peer connection lifecycle tracking.

mod direction;
mod registry;
mod resolver;
mod result;
mod state;

pub use direction::ConnectionDirection;
pub use registry::PeerRegistry;
pub use resolver::PeerResolver;
pub use result::ActivateResult;
pub use state::ConnectionState;

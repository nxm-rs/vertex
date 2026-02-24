//! Generic peer connection lifecycle tracking.

mod direction;
mod registry;
mod result;
mod state;

pub use direction::ConnectionDirection;
pub use registry::{PeerResolver, PeerRegistry, PeerRegistryStats};
pub use result::ActivateResult;
pub use state::ConnectionState;

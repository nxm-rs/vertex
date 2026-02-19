//! Lock-free peer scoring with atomics.

mod score;
mod snapshot;

pub use score::PeerScore;
pub use snapshot::PeerScoreSnapshot;

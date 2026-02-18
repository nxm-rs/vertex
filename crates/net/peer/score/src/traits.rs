//! Traits for scoring extensions.

use std::fmt::Debug;

use serde::{Deserialize, Serialize};

/// Protocol-specific extended scoring state.
///
/// Implement this to add custom scoring metrics to `PeerScore`. The state is stored
/// alongside the generic scoring metrics and uses atomics or interior mutability.
pub trait NetPeerScoreExt: Debug + Default + Send + Sync + 'static {
    /// Serializable snapshot type for persistence.
    type Snapshot: Clone
        + Debug
        + Default
        + Send
        + Sync
        + Serialize
        + for<'de> Deserialize<'de>
        + 'static;

    fn snapshot(&self) -> Self::Snapshot;
    fn restore(&self, snapshot: &Self::Snapshot);
}

impl NetPeerScoreExt for () {
    type Snapshot = ();
    fn snapshot(&self) -> Self::Snapshot {}
    fn restore(&self, _snapshot: &Self::Snapshot) {}
}

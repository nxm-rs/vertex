//! Traits for peer identifiers, scoring events, and persistable peer data.

use std::fmt::Debug;
use std::hash::Hash;

use serde::{Deserialize, Serialize};

/// Blanket-implemented for any type with Clone + Eq + Hash + Send + Sync + Debug + Serialize + Deserialize.
pub trait NetPeerId:
    Clone + Eq + Hash + Send + Sync + Debug + Serialize + for<'de> Deserialize<'de> + 'static
{
}

impl<T> NetPeerId for T where
    T: Clone + Eq + Hash + Send + Sync + Debug + Serialize + for<'de> Deserialize<'de> + 'static
{
}

/// Protocol-specific extended peer state.
///
/// Implement this trait to add custom state to `PeerState`. The state is stored
/// in a `RwLock` and accessed via `ext()` and `ext_mut()` methods.
///
/// # Example
///
/// ```ignore
/// #[derive(Debug, Default)]
/// struct SwarmExt {
///     overlay: Option<OverlayAddress>,
///     ethereum_address: Option<Address>,
/// }
///
/// impl NetPeerExt for SwarmExt {
///     type Snapshot = SwarmExtSnapshot;
///     fn snapshot(&self) -> Self::Snapshot { /* ... */ }
///     fn restore(&mut self, snapshot: &Self::Snapshot) { /* ... */ }
/// }
/// ```
pub trait NetPeerExt: Debug + Default + Send + Sync + 'static {
    /// Serializable snapshot type for persistence.
    type Snapshot: Clone + Debug + Send + Sync + Serialize + for<'de> Deserialize<'de> + 'static;

    /// Create a snapshot for persistence.
    fn snapshot(&self) -> Self::Snapshot;

    /// Restore state from a snapshot.
    fn restore(&mut self, snapshot: &Self::Snapshot);
}

/// No extended state (default). Uses `()` for both state and snapshot.
impl NetPeerExt for () {
    type Snapshot = ();

    fn snapshot(&self) -> Self::Snapshot {}

    fn restore(&mut self, _snapshot: &Self::Snapshot) {}
}

/// Scoring event with configurable weight (positive = good, negative = bad).
pub trait NetScoringEvent: Send + Sync + Debug + Clone + 'static {
    fn weight(&self) -> f64;

    fn latency_ms(&self) -> Option<u32> {
        None
    }

    fn is_connection_success(&self) -> bool {
        false
    }

    fn is_connection_timeout(&self) -> bool {
        false
    }

    fn is_protocol_error(&self) -> bool {
        false
    }
}

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

/// Persistable peer data bridge for storage backends.
pub trait NetPeerData<Id: NetPeerId>:
    Clone + Send + Sync + Debug + Serialize + for<'de> Deserialize<'de> + 'static
{
    fn id(&self) -> Id;
    fn multiaddrs(&self) -> &[libp2p::Multiaddr];
    fn is_banned(&self) -> bool;
    fn score(&self) -> f64;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    enum TestEvent {
        Success,
        Failure,
    }

    impl NetScoringEvent for TestEvent {
        fn weight(&self) -> f64 {
            match self {
                Self::Success => 1.0,
                Self::Failure => -1.0,
            }
        }
    }

    #[test]
    fn test_scoring_event() {
        assert!(TestEvent::Success.weight() > 0.0);
        assert!(TestEvent::Failure.weight() < 0.0);
    }
}

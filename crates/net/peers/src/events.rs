//! Peer events and non-blocking broadcast emitter.

use std::fmt::Debug;

use libp2p::PeerId;
use tokio::sync::broadcast;

use crate::state::ConnectionState;
use crate::traits::NetPeerId;

/// Peer manager events.
#[derive(Debug, Clone)]
pub enum PeerEvent<Id: NetPeerId> {
    Discovered {
        id: Id,
    },
    Connecting {
        id: Id,
    },
    Connected {
        id: Id,
        peer_id: Option<PeerId>,
    },
    Disconnected {
        id: Id,
        peer_id: Option<PeerId>,
    },
    Banned {
        id: Id,
        reason: Option<String>,
    },
    Unbanned {
        id: Id,
    },
    ScoreBelowThreshold {
        id: Id,
        score: f64,
        threshold: f64,
    },
    StateChanged {
        id: Id,
        old_state: ConnectionState,
        new_state: ConnectionState,
    },
}

impl<Id: NetPeerId> PeerEvent<Id> {
    pub fn peer_id(&self) -> &Id {
        match self {
            Self::Discovered { id }
            | Self::Connecting { id }
            | Self::Connected { id, .. }
            | Self::Disconnected { id, .. }
            | Self::Banned { id, .. }
            | Self::Unbanned { id }
            | Self::ScoreBelowThreshold { id, .. }
            | Self::StateChanged { id, .. } => id,
        }
    }

    pub fn is_connection_event(&self) -> bool {
        matches!(
            self,
            Self::Connecting { .. } | Self::Connected { .. } | Self::Disconnected { .. }
        )
    }

    pub fn is_ban_event(&self) -> bool {
        matches!(self, Self::Banned { .. } | Self::Unbanned { .. })
    }
}

const DEFAULT_CHANNEL_CAPACITY: usize = 256;

/// Non-blocking broadcast emitter. Slow subscribers drop events independently.
#[derive(Debug)]
pub struct EventEmitter<Id: NetPeerId> {
    tx: broadcast::Sender<PeerEvent<Id>>,
}

impl<Id: NetPeerId> Clone for EventEmitter<Id> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}

impl<Id: NetPeerId> Default for EventEmitter<Id> {
    fn default() -> Self {
        Self::new(DEFAULT_CHANNEL_CAPACITY)
    }
}

impl<Id: NetPeerId> EventEmitter<Id> {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    pub fn emit(&self, event: PeerEvent<Id>) {
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<PeerEvent<Id>> {
        self.tx.subscribe()
    }

    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl<Id: NetPeerId> EventEmitter<Id> {
    pub fn peer_discovered(&self, id: Id) {
        self.emit(PeerEvent::Discovered { id });
    }

    pub fn peer_connecting(&self, id: Id) {
        self.emit(PeerEvent::Connecting { id });
    }

    pub fn peer_connected(&self, id: Id, peer_id: Option<PeerId>) {
        self.emit(PeerEvent::Connected { id, peer_id });
    }

    pub fn peer_disconnected(&self, id: Id, peer_id: Option<PeerId>) {
        self.emit(PeerEvent::Disconnected { id, peer_id });
    }

    pub fn peer_banned(&self, id: Id, reason: Option<String>) {
        self.emit(PeerEvent::Banned { id, reason });
    }

    pub fn peer_unbanned(&self, id: Id) {
        self.emit(PeerEvent::Unbanned { id });
    }

    pub fn state_changed(&self, id: Id, old_state: ConnectionState, new_state: ConnectionState) {
        self.emit(PeerEvent::StateChanged {
            id,
            old_state,
            new_state,
        });
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
    struct TestId(u64);

    #[tokio::test]
    async fn test_event_emitter_basic() {
        let emitter = EventEmitter::<TestId>::default();
        let mut rx = emitter.subscribe();

        emitter.peer_discovered(TestId(1));

        let event = rx.recv().await.unwrap();
        match event {
            PeerEvent::Discovered { id } => assert_eq!(id, TestId(1)),
            _ => panic!("unexpected event"),
        }
    }

    #[tokio::test]
    async fn test_event_emitter_multiple_subscribers() {
        let emitter = EventEmitter::<TestId>::default();
        let mut rx1 = emitter.subscribe();
        let mut rx2 = emitter.subscribe();

        emitter.peer_connected(TestId(1), None);

        // Both subscribers should receive the event
        let event1 = rx1.recv().await.unwrap();
        let event2 = rx2.recv().await.unwrap();

        match (&event1, &event2) {
            (PeerEvent::Connected { id: id1, .. }, PeerEvent::Connected { id: id2, .. }) => {
                assert_eq!(*id1, TestId(1));
                assert_eq!(*id2, TestId(1));
            }
            _ => panic!("unexpected events"),
        }
    }

    #[test]
    fn test_event_emitter_no_subscribers() {
        let emitter = EventEmitter::<TestId>::default();

        // Should not panic even with no subscribers
        emitter.peer_discovered(TestId(1));
        emitter.peer_banned(TestId(1), Some("test".to_string()));
    }

    #[test]
    fn test_event_emitter_subscriber_count() {
        let emitter = EventEmitter::<TestId>::default();
        assert_eq!(emitter.subscriber_count(), 0);

        let _rx1 = emitter.subscribe();
        assert_eq!(emitter.subscriber_count(), 1);

        let _rx2 = emitter.subscribe();
        assert_eq!(emitter.subscriber_count(), 2);

        drop(_rx1);
        // Note: subscriber count may not immediately reflect drops
    }

    #[test]
    fn test_peer_event_methods() {
        let event = PeerEvent::Connected {
            id: TestId(1),
            peer_id: None,
        };

        assert_eq!(*event.peer_id(), TestId(1));
        assert!(event.is_connection_event());
        assert!(!event.is_ban_event());

        let ban_event = PeerEvent::Banned {
            id: TestId(1),
            reason: None,
        };
        assert!(ban_event.is_ban_event());
        assert!(!ban_event.is_connection_event());
    }
}

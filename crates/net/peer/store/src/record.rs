//! Peer record with ID, data, and timestamps.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::backoff::BackoffState;
use crate::traits::{DataBounds, NetPeerId};

/// Current unix timestamp in seconds.
pub fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Peer record stored in a peer store.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(
    serialize = "Id: NetPeerId, Data: Serialize",
    deserialize = "Id: NetPeerId, Data: for<'a> Deserialize<'a>"
))]
pub struct PeerRecord<Id: NetPeerId, Data: DataBounds = ()> {
    pub id: Id,
    pub data: Data,
    pub first_seen: u64,
    pub last_seen: u64,
    pub last_dial_attempt: u64,
    pub consecutive_failures: u32,
    pub is_banned: bool,
}

impl<Id: NetPeerId, Data: DataBounds> PeerRecord<Id, Data> {
    /// Construct a `BackoffState` from this record's fields.
    pub fn backoff_state(&self) -> BackoffState {
        BackoffState::new(self.last_dial_attempt, self.consecutive_failures)
    }

    /// Update last_seen to current time.
    pub fn touch(&mut self) {
        self.last_seen = unix_timestamp_secs();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
    struct TestId(u64);

    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    struct TestData {
        name: String,
    }

    fn test_record(id: u64, name: &str, first_seen: u64, last_seen: u64) -> PeerRecord<TestId, TestData> {
        PeerRecord {
            id: TestId(id),
            data: TestData { name: name.into() },
            first_seen,
            last_seen,
            last_dial_attempt: 0,
            consecutive_failures: 0,
            is_banned: false,
        }
    }

    #[test]
    fn test_fields() {
        let record = test_record(1, "test", 100, 200);
        assert_eq!(record.id, TestId(1));
        assert_eq!(record.data.name, "test");
        assert_eq!(record.first_seen, 100);
        assert_eq!(record.last_seen, 200);
        assert_eq!(record.last_dial_attempt, 0);
        assert_eq!(record.consecutive_failures, 0);
        assert!(!record.is_banned);
    }

    #[test]
    fn test_touch() {
        let mut record = test_record(1, "", 100, 100);
        std::thread::sleep(std::time::Duration::from_millis(10));
        record.touch();
        assert!(record.last_seen >= record.first_seen);
    }

    #[test]
    fn test_serialization() {
        let record = test_record(1, "test", 100, 200);
        let json = serde_json::to_string(&record).unwrap();
        let restored: PeerRecord<TestId, TestData> = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.id, record.id);
        assert_eq!(restored.data.name, record.data.name);
    }

    #[test]
    fn test_backoff_state_from_record() {
        let record = PeerRecord {
            id: TestId(1),
            data: TestData::default(),
            first_seen: 100,
            last_seen: 200,
            last_dial_attempt: 150,
            consecutive_failures: 3,
            is_banned: false,
        };
        let state = record.backoff_state();
        assert_eq!(state.last_attempt, 150);
        assert_eq!(state.consecutive_failures, 3);
    }
}

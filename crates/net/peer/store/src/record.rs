//! Peer record with ID, data, and timestamps.

use serde::{Deserialize, Serialize};

use crate::traits::{DataBounds, NetPeerId};

/// Peer record stored in a peer store.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(
    serialize = "Id: NetPeerId, Data: Serialize",
    deserialize = "Id: NetPeerId, Data: for<'a> Deserialize<'a>"
))]
pub struct PeerRecord<Id: NetPeerId, Data: DataBounds = ()> {
    id: Id,
    data: Data,
    first_seen: u64,
    last_seen: u64,
}

impl<Id: NetPeerId, Data: DataBounds> PeerRecord<Id, Data> {
    pub fn new(id: Id, data: Data, first_seen: u64, last_seen: u64) -> Self {
        Self {
            id,
            data,
            first_seen,
            last_seen,
        }
    }

    /// Create with current timestamp for both first_seen and last_seen.
    pub fn new_now(id: Id, data: Data) -> Self {
        let now = unix_timestamp_secs();
        Self::new(id, data, now, now)
    }

    pub fn id(&self) -> &Id {
        &self.id
    }

    pub fn data(&self) -> &Data {
        &self.data
    }

    pub fn data_mut(&mut self) -> &mut Data {
        &mut self.data
    }

    pub fn first_seen(&self) -> u64 {
        self.first_seen
    }

    pub fn last_seen(&self) -> u64 {
        self.last_seen
    }

    /// Update last_seen to current time.
    pub fn touch(&mut self) {
        self.last_seen = unix_timestamp_secs();
    }

    pub fn set_last_seen(&mut self, timestamp: u64) {
        self.last_seen = timestamp;
    }

    pub fn set_data(&mut self, data: Data) {
        self.data = data;
    }

    pub fn into_id(self) -> Id {
        self.id
    }

    pub fn into_data(self) -> Data {
        self.data
    }

    pub fn into_parts(self) -> (Id, Data) {
        (self.id, self.data)
    }
}

fn unix_timestamp_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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

    #[test]
    fn test_new() {
        let record = PeerRecord::new(TestId(1), TestData { name: "test".into() }, 100, 200);
        assert_eq!(record.id(), &TestId(1));
        assert_eq!(record.data().name, "test");
        assert_eq!(record.first_seen(), 100);
        assert_eq!(record.last_seen(), 200);
    }

    #[test]
    fn test_new_now() {
        let record = PeerRecord::new_now(TestId(1), TestData::default());
        assert!(record.first_seen() > 0);
        assert_eq!(record.first_seen(), record.last_seen());
    }

    #[test]
    fn test_touch() {
        let mut record = PeerRecord::new(TestId(1), TestData::default(), 100, 100);
        std::thread::sleep(std::time::Duration::from_millis(10));
        record.touch();
        assert!(record.last_seen() >= record.first_seen());
    }

    #[test]
    fn test_into_parts() {
        let record = PeerRecord::new(TestId(42), TestData { name: "foo".into() }, 0, 0);
        let (id, data) = record.into_parts();
        assert_eq!(id, TestId(42));
        assert_eq!(data.name, "foo");
    }

    #[test]
    fn test_serialization() {
        let record = PeerRecord::new(TestId(1), TestData { name: "test".into() }, 100, 200);
        let json = serde_json::to_string(&record).unwrap();
        let restored: PeerRecord<TestId, TestData> = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.id(), record.id());
        assert_eq!(restored.data().name, record.data().name);
    }
}

//! Dial tracking for in-progress connection attempts.

use std::collections::HashMap;
use std::time::Instant;

use libp2p::{Multiaddr, PeerId};
use parking_lot::Mutex;

/// Information about an active dial attempt.
#[derive(Debug, Clone)]
pub struct DialInfo {
    /// The multiaddr currently being dialed.
    pub addr: Multiaddr,
    /// Remaining multiaddrs to try if current one fails.
    pub remaining_addrs: Vec<Multiaddr>,
    /// PeerId (set after ConnectionEstablished, before handshake).
    pub peer_id: Option<PeerId>,
    /// Whether this dial was initiated for gossip exchange.
    pub for_gossip: bool,
    /// When the dial was started.
    pub started_at: Instant,
}

/// Tracks in-progress dial attempts by multiaddr.
///
/// The DialTracker owns the "connecting" state - peers are not added to
/// PeerManager until handshake completes and SwarmPeer is known.
pub struct DialTracker {
    /// Active dials indexed by current multiaddr.
    dials: Mutex<HashMap<Multiaddr, DialInfo>>,
}

impl DialTracker {
    pub fn new() -> Self {
        Self {
            dials: Mutex::new(HashMap::new()),
        }
    }

    /// Start tracking a dial with one or more addresses.
    ///
    /// Returns the first address to dial, or None if no addresses provided
    /// or if already dialing this address.
    pub fn start_dial(&self, addrs: Vec<Multiaddr>, for_gossip: bool) -> Option<Multiaddr> {
        if addrs.is_empty() {
            return None;
        }

        let mut addrs = addrs;
        let addr = addrs.remove(0);

        let mut dials = self.dials.lock();
        if dials.contains_key(&addr) {
            return None;
        }

        let info = DialInfo {
            addr: addr.clone(),
            remaining_addrs: addrs,
            peer_id: None,
            for_gossip,
            started_at: Instant::now(),
        };

        dials.insert(addr.clone(), info);
        Some(addr)
    }

    /// Associate a PeerId with a dial after ConnectionEstablished.
    pub fn associate_peer_id(&self, addr: &Multiaddr, peer_id: PeerId) {
        if let Some(info) = self.dials.lock().get_mut(addr) {
            info.peer_id = Some(peer_id);
        }
    }

    /// Try the next address after current one fails.
    ///
    /// Returns the next address to dial, or None if no more addresses.
    pub fn try_next_addr(&self, current_addr: &Multiaddr) -> Option<Multiaddr> {
        let mut dials = self.dials.lock();

        let info = dials.remove(current_addr)?;
        if info.remaining_addrs.is_empty() {
            return None;
        }

        let mut remaining = info.remaining_addrs;
        let next_addr = remaining.remove(0);

        let new_info = DialInfo {
            addr: next_addr.clone(),
            remaining_addrs: remaining,
            peer_id: info.peer_id,
            for_gossip: info.for_gossip,
            started_at: Instant::now(),
        };

        dials.insert(next_addr.clone(), new_info);
        Some(next_addr)
    }

    /// Get dial info by address.
    pub fn get(&self, addr: &Multiaddr) -> Option<DialInfo> {
        self.dials.lock().get(addr).cloned()
    }

    /// Get dial info by PeerId.
    pub fn get_by_peer_id(&self, peer_id: &PeerId) -> Option<DialInfo> {
        self.dials
            .lock()
            .values()
            .find(|info| info.peer_id.as_ref() == Some(peer_id))
            .cloned()
    }

    /// Find address by PeerId.
    pub fn find_addr_by_peer_id(&self, peer_id: &PeerId) -> Option<Multiaddr> {
        self.get_by_peer_id(peer_id).map(|info| info.addr)
    }

    /// Complete a dial (success or failure) by address.
    pub fn complete_dial(&self, addr: &Multiaddr) -> Option<DialInfo> {
        self.dials.lock().remove(addr)
    }

    /// Complete a dial by PeerId.
    pub fn complete_dial_by_peer_id(&self, peer_id: &PeerId) -> Option<DialInfo> {
        let mut dials = self.dials.lock();
        let addr = dials
            .values()
            .find(|info| info.peer_id.as_ref() == Some(peer_id))
            .map(|info| info.addr.clone())?;
        dials.remove(&addr)
    }

    /// Check if a dial is in progress for an address.
    pub fn is_dialing(&self, addr: &Multiaddr) -> bool {
        self.dials.lock().contains_key(addr)
    }

    /// Check if a dial is in progress for a PeerId.
    pub fn is_dialing_peer(&self, peer_id: &PeerId) -> bool {
        self.dials
            .lock()
            .values()
            .any(|info| info.peer_id.as_ref() == Some(peer_id))
    }

    /// Get the count of pending dial attempts.
    pub fn pending_count(&self) -> usize {
        self.dials.lock().len()
    }
}

impl Default for DialTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(port: u16) -> Multiaddr {
        format!("/ip4/127.0.0.1/tcp/{}", port).parse().unwrap()
    }

    #[allow(dead_code)]
    fn test_addr_with_peer(port: u16, peer_id: &PeerId) -> Multiaddr {
        format!("/ip4/127.0.0.1/tcp/{}/p2p/{}", port, peer_id)
            .parse()
            .unwrap()
    }

    #[test]
    fn test_start_dial_single_addr() {
        let tracker = DialTracker::new();
        let addr = test_addr(9000);

        let started = tracker.start_dial(vec![addr.clone()], false);
        assert_eq!(started, Some(addr.clone()));
        assert!(tracker.is_dialing(&addr));
        assert_eq!(tracker.pending_count(), 1);

        // Can't start duplicate
        let duplicate = tracker.start_dial(vec![addr.clone()], false);
        assert!(duplicate.is_none());
    }

    #[test]
    fn test_start_dial_empty_addrs() {
        let tracker = DialTracker::new();
        let started = tracker.start_dial(vec![], false);
        assert!(started.is_none());
        assert_eq!(tracker.pending_count(), 0);
    }

    #[test]
    fn test_associate_peer_id() {
        let tracker = DialTracker::new();
        let addr = test_addr(9001);
        let peer_id = PeerId::random();

        tracker.start_dial(vec![addr.clone()], true);

        // Before association
        let info = tracker.get(&addr).unwrap();
        assert!(info.peer_id.is_none());
        assert!(info.for_gossip);

        // Associate peer_id
        tracker.associate_peer_id(&addr, peer_id);

        // After association
        let info = tracker.get(&addr).unwrap();
        assert_eq!(info.peer_id, Some(peer_id));
        assert!(tracker.is_dialing_peer(&peer_id));
    }

    #[test]
    fn test_complete_dial() {
        let tracker = DialTracker::new();
        let addr = test_addr(9002);

        tracker.start_dial(vec![addr.clone()], false);
        assert!(tracker.is_dialing(&addr));

        let info = tracker.complete_dial(&addr).unwrap();
        assert_eq!(info.addr, addr);
        assert!(!tracker.is_dialing(&addr));
        assert_eq!(tracker.pending_count(), 0);
    }

    #[test]
    fn test_complete_dial_by_peer_id() {
        let tracker = DialTracker::new();
        let addr = test_addr(9003);
        let peer_id = PeerId::random();

        tracker.start_dial(vec![addr.clone()], false);
        tracker.associate_peer_id(&addr, peer_id);

        let info = tracker.complete_dial_by_peer_id(&peer_id).unwrap();
        assert_eq!(info.addr, addr);
        assert_eq!(info.peer_id, Some(peer_id));
        assert!(!tracker.is_dialing(&addr));
    }

    #[test]
    fn test_multi_addr_dial() {
        let tracker = DialTracker::new();
        let addr1 = test_addr(9010);
        let addr2 = test_addr(9011);
        let addr3 = test_addr(9012);

        let started = tracker.start_dial(vec![addr1.clone(), addr2.clone(), addr3.clone()], false);
        assert_eq!(started, Some(addr1.clone()));
        assert!(tracker.is_dialing(&addr1));

        // First addr fails, try next
        let next = tracker.try_next_addr(&addr1);
        assert_eq!(next, Some(addr2.clone()));
        assert!(!tracker.is_dialing(&addr1));
        assert!(tracker.is_dialing(&addr2));

        // Second addr fails, try next
        let next = tracker.try_next_addr(&addr2);
        assert_eq!(next, Some(addr3.clone()));

        // Third addr fails, no more
        let next = tracker.try_next_addr(&addr3);
        assert!(next.is_none());
        assert!(!tracker.is_dialing(&addr3));
        assert_eq!(tracker.pending_count(), 0);
    }

    #[test]
    fn test_multi_addr_dial_success_on_second() {
        let tracker = DialTracker::new();
        let addr1 = test_addr(9020);
        let addr2 = test_addr(9021);

        tracker.start_dial(vec![addr1.clone(), addr2.clone()], false);

        // First fails
        let next = tracker.try_next_addr(&addr1).unwrap();
        assert_eq!(next, addr2);

        // Second succeeds
        let info = tracker.complete_dial(&addr2).unwrap();
        assert_eq!(info.addr, addr2);
        assert!(info.remaining_addrs.is_empty());
    }

    #[test]
    fn test_peer_id_preserved_across_retry() {
        let tracker = DialTracker::new();
        let addr1 = test_addr(9030);
        let addr2 = test_addr(9031);
        let peer_id = PeerId::random();

        tracker.start_dial(vec![addr1.clone(), addr2.clone()], true);
        tracker.associate_peer_id(&addr1, peer_id);

        // Retry should preserve peer_id
        let next = tracker.try_next_addr(&addr1).unwrap();
        assert_eq!(next, addr2);

        let info = tracker.get(&addr2).unwrap();
        assert_eq!(info.peer_id, Some(peer_id));
        assert!(info.for_gossip);
    }

    #[test]
    fn test_find_addr_by_peer_id() {
        let tracker = DialTracker::new();
        let addr = test_addr(9040);
        let peer_id = PeerId::random();

        tracker.start_dial(vec![addr.clone()], false);
        tracker.associate_peer_id(&addr, peer_id);

        let found = tracker.find_addr_by_peer_id(&peer_id);
        assert_eq!(found, Some(addr));

        let not_found = tracker.find_addr_by_peer_id(&PeerId::random());
        assert!(not_found.is_none());
    }
}

//! Lock-free peer state with atomics for hot paths and per-peer RwLocks for cold paths.

use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::Duration;

use libp2p::Multiaddr;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::score::{PeerScore, PeerScoreSnapshot};
use crate::traits::{NetPeerExt, NetPeerId, NetPeerScoreExt};

/// Peer connection state (stored as u8 for atomic operations).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum ConnectionState {
    Known = 0,
    Connecting = 1,
    Connected = 2,
    Disconnected = 3,
    Banned = 4,
}

impl ConnectionState {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Known,
            1 => Self::Connecting,
            2 => Self::Connected,
            3 => Self::Disconnected,
            4 => Self::Banned,
            _ => Self::Known,
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self, ConnectionState::Connected)
    }

    pub fn is_dialable(&self) -> bool {
        matches!(self, ConnectionState::Known | ConnectionState::Disconnected)
    }

    pub fn is_banned(&self) -> bool {
        matches!(self, ConnectionState::Banned)
    }
}

/// Ban metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BanInfo {
    pub banned_at_unix: u64,
    pub reason: Option<String>,
}

/// Lock-free peer state using atomics for hot paths, per-peer RwLock for cold paths.
///
/// The `Ext` type parameter allows protocols to add custom state (stored in RwLock).
/// The `ScoreExt` type parameter allows protocols to add custom scoring metrics.
#[derive(Debug)]
pub struct PeerState<Id: NetPeerId, Ext: NetPeerExt = (), ScoreExt: NetPeerScoreExt = ()> {
    _marker: PhantomData<Id>,

    /// Scoring metrics (Arc for cheap sharing with protocol handlers).
    scoring: Arc<PeerScore<ScoreExt>>,

    state: AtomicU8,
    first_seen: AtomicU64,
    last_seen: AtomicU64,

    multiaddrs: RwLock<Vec<Multiaddr>>,
    ban_info: RwLock<Option<BanInfo>>,

    /// Protocol-specific extended state.
    ext: RwLock<Ext>,
}

impl<Id: NetPeerId, Ext: NetPeerExt, ScoreExt: NetPeerScoreExt> PeerState<Id, Ext, ScoreExt> {
    pub fn new() -> Self {
        let now = current_unix_timestamp();
        Self {
            _marker: PhantomData,
            scoring: Arc::new(PeerScore::new()),
            state: AtomicU8::new(ConnectionState::Known as u8),
            first_seen: AtomicU64::new(now),
            last_seen: AtomicU64::new(now),
            multiaddrs: RwLock::new(Vec::new()),
            ban_info: RwLock::new(None),
            ext: RwLock::new(Ext::default()),
        }
    }

    pub fn with_multiaddrs(multiaddrs: Vec<Multiaddr>) -> Self {
        Self {
            multiaddrs: RwLock::new(multiaddrs),
            ..Self::new()
        }
    }

    pub fn with_ext(ext: Ext) -> Self {
        Self {
            ext: RwLock::new(ext),
            ..Self::new()
        }
    }

    pub fn with_multiaddrs_and_ext(multiaddrs: Vec<Multiaddr>, ext: Ext) -> Self {
        Self {
            multiaddrs: RwLock::new(multiaddrs),
            ext: RwLock::new(ext),
            ..Self::new()
        }
    }

    /// Get a clone of the scoring Arc for sharing with protocol handlers.
    pub fn scoring(&self) -> Arc<PeerScore<ScoreExt>> {
        Arc::clone(&self.scoring)
    }

    pub fn score(&self) -> f64 {
        self.scoring.score()
    }

    pub fn add_score(&self, delta: f64) {
        self.scoring.add_score(delta);
    }

    pub fn set_score(&self, score: f64) {
        self.scoring.set_score(score);
    }

    pub fn should_ban(&self, threshold: f64) -> bool {
        self.scoring.should_ban(threshold)
    }

    pub fn connection_successes(&self) -> u32 {
        self.scoring.connection_successes()
    }

    pub fn connection_timeouts(&self) -> u32 {
        self.scoring.connection_timeouts()
    }

    pub fn connection_refusals(&self) -> u32 {
        self.scoring.connection_refusals()
    }

    pub fn handshake_failures(&self) -> u32 {
        self.scoring.handshake_failures()
    }

    pub fn protocol_errors(&self) -> u32 {
        self.scoring.protocol_errors()
    }

    /// Record success: increments counter, updates latency, adds +1.0 score.
    pub fn record_success(&self, latency: Duration) {
        self.scoring.record_success(latency.as_nanos() as u64);
        self.scoring.add_score(1.0);
        self.touch();
    }

    /// Record timeout: increments counter, adds -1.5 score.
    pub fn record_timeout(&self) {
        self.scoring.record_timeout();
        self.scoring.add_score(-1.5);
    }

    /// Record refusal: increments counter, adds -1.0 score.
    pub fn record_refusal(&self) {
        self.scoring.record_refusal();
        self.scoring.add_score(-1.0);
    }

    /// Record handshake failure: increments counter, adds -5.0 score.
    pub fn record_handshake_failure(&self) {
        self.scoring.record_handshake_failure();
        self.scoring.add_score(-5.0);
    }

    /// Record protocol error: increments counter, adds -3.0 score.
    pub fn record_protocol_error(&self) {
        self.scoring.record_protocol_error();
        self.scoring.add_score(-3.0);
    }

    pub fn latency(&self) -> Option<Duration> {
        self.scoring.avg_latency()
    }

    pub fn set_latency(&self, latency: Duration) {
        self.scoring.record_latency(latency.as_nanos() as u64);
    }

    pub fn connection_state(&self) -> ConnectionState {
        ConnectionState::from_u8(self.state.load(Ordering::Relaxed))
    }

    pub fn is_connected(&self) -> bool {
        self.connection_state().is_connected()
    }

    pub fn is_dialable(&self) -> bool {
        self.connection_state().is_dialable()
    }

    pub fn is_banned(&self) -> bool {
        self.connection_state().is_banned()
    }

    pub fn first_seen(&self) -> u64 {
        self.first_seen.load(Ordering::Relaxed)
    }

    pub fn last_seen(&self) -> u64 {
        self.last_seen.load(Ordering::Relaxed)
    }

    pub fn set_connection_state(&self, state: ConnectionState) {
        self.state.store(state as u8, Ordering::Relaxed);
        self.touch();
    }

    pub fn touch(&self) {
        self.last_seen
            .store(current_unix_timestamp(), Ordering::Relaxed);
    }

    pub fn multiaddrs(&self) -> Vec<Multiaddr> {
        self.multiaddrs.read().clone()
    }

    pub fn update_multiaddrs(&self, addrs: Vec<Multiaddr>) {
        *self.multiaddrs.write() = addrs;
    }

    pub fn add_multiaddrs(&self, addrs: impl IntoIterator<Item = Multiaddr>) {
        let mut current = self.multiaddrs.write();
        for addr in addrs {
            if !current.contains(&addr) {
                current.push(addr);
            }
        }
    }

    pub fn ban_info(&self) -> Option<BanInfo> {
        self.ban_info.read().clone()
    }

    pub fn ban(&self, reason: Option<String>) {
        self.set_connection_state(ConnectionState::Banned);
        *self.ban_info.write() = Some(BanInfo {
            banned_at_unix: current_unix_timestamp(),
            reason,
        });
    }

    pub fn unban(&self) {
        self.set_connection_state(ConnectionState::Disconnected);
        *self.ban_info.write() = None;
    }

    pub fn ext(&self) -> parking_lot::RwLockReadGuard<'_, Ext> {
        self.ext.read()
    }

    pub fn ext_mut(&self) -> parking_lot::RwLockWriteGuard<'_, Ext> {
        self.ext.write()
    }

    pub fn set_ext(&self, ext: Ext) {
        *self.ext.write() = ext;
    }

    /// Create a serializable snapshot. ID must be provided since it's not stored here.
    pub fn snapshot(&self, id: Id) -> NetPeerSnapshot<Id, Ext::Snapshot, ScoreExt::Snapshot> {
        NetPeerSnapshot {
            id,
            scoring: self.scoring.snapshot(),
            state: self.connection_state(),
            first_seen: self.first_seen(),
            last_seen: self.last_seen(),
            multiaddrs: self.multiaddrs(),
            ban_info: self.ban_info(),
            ext: self.ext.read().snapshot(),
        }
    }

    pub fn restore(&self, snapshot: &NetPeerSnapshot<Id, Ext::Snapshot, ScoreExt::Snapshot>) {
        self.scoring.restore(&snapshot.scoring);
        self.state.store(snapshot.state as u8, Ordering::Relaxed);
        self.first_seen
            .store(snapshot.first_seen, Ordering::Relaxed);
        self.last_seen.store(snapshot.last_seen, Ordering::Relaxed);
        *self.multiaddrs.write() = snapshot.multiaddrs.clone();
        *self.ban_info.write() = snapshot.ban_info.clone();
        self.ext.write().restore(&snapshot.ext);
    }
}

impl<Id: NetPeerId, Ext: NetPeerExt, ScoreExt: NetPeerScoreExt> Default
    for PeerState<Id, Ext, ScoreExt>
{
    fn default() -> Self {
        Self::new()
    }
}

/// Serializable peer state snapshot for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(
    serialize = "Id: NetPeerId, ExtSnap: Serialize, ScoreExtSnap: Serialize",
    deserialize = "Id: NetPeerId, ExtSnap: for<'a> Deserialize<'a>, ScoreExtSnap: for<'a> Deserialize<'a>"
))]
pub struct NetPeerSnapshot<Id: NetPeerId, ExtSnap = (), ScoreExtSnap = ()> {
    pub id: Id,
    pub scoring: PeerScoreSnapshot<ScoreExtSnap>,
    pub state: ConnectionState,
    #[serde(default)]
    pub first_seen: u64,
    pub last_seen: u64,
    pub multiaddrs: Vec<Multiaddr>,
    pub ban_info: Option<BanInfo>,
    pub ext: ExtSnap,
}

impl<Id: NetPeerId, ExtSnap, ScoreExtSnap> NetPeerSnapshot<Id, ExtSnap, ScoreExtSnap> {
    /// Convert snapshot back to a PeerState. Returns (id, state) tuple.
    pub fn into_state<Ext, ScoreExt>(self) -> (Id, PeerState<Id, Ext, ScoreExt>)
    where
        Ext: NetPeerExt<Snapshot = ExtSnap>,
        ScoreExt: NetPeerScoreExt<Snapshot = ScoreExtSnap>,
    {
        let state = PeerState::new();
        state.restore(&self);
        (self.id, state)
    }

    pub fn score(&self) -> f64 {
        self.scoring.score
    }

    pub fn connection_successes(&self) -> u32 {
        self.scoring.connection_successes
    }

    pub fn connection_timeouts(&self) -> u32 {
        self.scoring.connection_timeouts
    }

    pub fn protocol_errors(&self) -> u32 {
        self.scoring.protocol_errors
    }
}

fn current_unix_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
    struct TestId(u64);

    #[test]
    fn test_peer_state_new() {
        let state: PeerState<TestId> = PeerState::new();

        assert_eq!(state.score(), 0.0);
        assert_eq!(state.connection_state(), ConnectionState::Known);
        assert!(!state.is_connected());
        assert!(state.is_dialable());
        assert!(!state.is_banned());
        assert!(state.latency().is_none());
    }

    #[test]
    fn test_score_operations() {
        let state: PeerState<TestId> = PeerState::new();

        state.add_score(10.0);
        assert!((state.score() - 10.0).abs() < 0.001);

        state.add_score(-5.0);
        assert!((state.score() - 5.0).abs() < 0.001);

        state.set_score(100.0);
        assert!((state.score() - 100.0).abs() < 0.001);
    }

    #[test]
    fn test_connection_state() {
        let state: PeerState<TestId> = PeerState::new();

        state.set_connection_state(ConnectionState::Connecting);
        assert_eq!(state.connection_state(), ConnectionState::Connecting);
        assert!(!state.is_connected());
        assert!(!state.is_dialable());

        state.set_connection_state(ConnectionState::Connected);
        assert!(state.is_connected());

        state.set_connection_state(ConnectionState::Disconnected);
        assert!(!state.is_connected());
        assert!(state.is_dialable());
    }

    #[test]
    fn test_record_operations() {
        let state: PeerState<TestId> = PeerState::new();

        state.record_success(Duration::from_millis(50));
        assert_eq!(state.connection_successes(), 1);
        assert!(state.latency().is_some());
        assert!(state.score() > 0.0);

        state.record_timeout();
        assert_eq!(state.connection_timeouts(), 1);

        state.record_refusal();
        assert_eq!(state.connection_refusals(), 1);

        state.record_handshake_failure();
        assert_eq!(state.handshake_failures(), 1);

        state.record_protocol_error();
        assert_eq!(state.protocol_errors(), 1);
    }

    #[test]
    fn test_multiaddrs() {
        let state: PeerState<TestId> = PeerState::new();

        let addr1: Multiaddr = "/ip4/127.0.0.1/tcp/1634".parse().unwrap();
        let addr2: Multiaddr = "/ip4/127.0.0.2/tcp/1634".parse().unwrap();

        state.update_multiaddrs(vec![addr1.clone()]);
        assert_eq!(state.multiaddrs().len(), 1);

        state.add_multiaddrs(vec![addr2.clone()]);
        assert_eq!(state.multiaddrs().len(), 2);

        state.add_multiaddrs(vec![addr1.clone()]);
        assert_eq!(state.multiaddrs().len(), 2);
    }

    #[test]
    fn test_ban_unban() {
        let state: PeerState<TestId> = PeerState::new();

        state.ban(Some("test reason".to_string()));
        assert!(state.is_banned());
        assert!(state.ban_info().is_some());
        assert_eq!(
            state.ban_info().unwrap().reason,
            Some("test reason".to_string())
        );

        state.unban();
        assert!(!state.is_banned());
        assert!(state.ban_info().is_none());
    }

    #[test]
    fn test_scoring_arc() {
        let state: PeerState<TestId> = PeerState::new();

        let scoring = state.scoring();

        scoring.add_score(50.0);
        assert!((state.score() - 50.0).abs() < 0.001);

        state.add_score(25.0);
        assert!((scoring.score() - 75.0).abs() < 0.001);
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let state: PeerState<TestId> = PeerState::new();
        state.set_score(75.5);
        state.set_connection_state(ConnectionState::Connected);
        state.record_success(Duration::from_millis(100));
        state.update_multiaddrs(vec!["/ip4/127.0.0.1/tcp/1634".parse().unwrap()]);

        let snapshot = state.snapshot(TestId(42));
        assert_eq!(snapshot.id, TestId(42));
        assert!((snapshot.score() - 76.5).abs() < 0.1);
        assert_eq!(snapshot.state, ConnectionState::Connected);

        let (restored_id, restored): (TestId, PeerState<TestId>) = snapshot.into_state();
        assert_eq!(restored_id, TestId(42));
        assert!((restored.score() - state.score()).abs() < 0.001);
        assert_eq!(
            restored.connection_successes(),
            state.connection_successes()
        );
    }

    #[test]
    fn test_concurrent_score_updates() {
        use std::thread;

        let state: Arc<PeerState<TestId>> = Arc::new(PeerState::new());
        let mut handles = vec![];

        for _ in 0..10 {
            let state_clone = Arc::clone(&state);
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    state_clone.add_score(1.0);
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert!((state.score() - 1000.0).abs() < 1.0);
    }
}

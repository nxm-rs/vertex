//! Periodic maintenance: score decay, ban expiry, stale-peer purging, and
//! snapshot persistence.

use metrics::gauge;
use std::sync::atomic::Ordering;
use tracing::{debug, info, warn};
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_primitives::OverlayAddress;

use crate::entry::{PeerEntry, PeerSnapshot, on_health_added, unix_timestamp_secs};
use crate::manager::PeerManager;

/// Drop snapshot records last seen longer ago than this at store time (7 days).
///
/// `last_seen` is stamped on the runtime bookkeeping clock, so this compares
/// like against like across a restart (persisted seconds versus current
/// seconds on the same clock). The runtime stale purge only drops peers that
/// are actively failing; a verified peer that simply goes quiet is never
/// purged in-process, so without this bound the persisted table would hoard
/// records unreachable for weeks and spend bin-admission capacity that the
/// freshest-first restore owes to live peers. A week keeps peers across a
/// realistic downtime while discarding the long-dead. A currently-connected
/// peer is exempt: the live connection is proof of reachability regardless of
/// how long ago `last_seen` was stamped.
const SNAPSHOT_STALE_SECS: u64 = 7 * 24 * 3600;

/// Whether a record last seen at `last_seen` is fresh enough to persist at the
/// current runtime-clock time `now`. A `last_seen` ahead of `now` (clock skew)
/// counts as fresh.
fn snapshot_is_fresh(now: u64, last_seen: u64) -> bool {
    now.saturating_sub(last_seen) <= SNAPSHOT_STALE_SECS
}

impl<I: SwarmIdentity> PeerManager<I> {
    /// Single periodic entry point, driven from outside the crate (see
    /// [`crate::spawn_peer_manager_task`]).
    ///
    /// In order: decays every peer's score toward zero (10 minute half-life
    /// disconnected, 5 minutes connected), lifts expired timed bans
    /// (resetting the score to the disconnect threshold and emitting
    /// `Unbanned`), purges stale never-connected peers, and
    /// writes a snapshot when one is due
    /// ([`PeerManagerConfig::snapshot_interval`](crate::PeerManagerConfig)
    /// since the last write). `now_unix_secs` is injected so tests can drive
    /// the schedule without a clock.
    pub fn tick(&self, now_unix_secs: u64) {
        self.decay_scores(now_unix_secs);
        self.expire_bans(now_unix_secs);
        self.purge_stale();

        if self.store.is_none() {
            return;
        }
        let last = self.last_snapshot.load(Ordering::Acquire);
        if now_unix_secs.saturating_sub(last) < self.snapshot_interval.as_secs() {
            return;
        }
        // CAS so concurrent ticks write at most one snapshot per interval.
        if self
            .last_snapshot
            .compare_exchange(last, now_unix_secs, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.snapshot();
        }
    }

    /// Write the verified peer set to the snapshot store (no-op without one).
    ///
    /// Called by [`Self::tick`] on schedule and by topology on graceful
    /// shutdown so the final state is not lost to the snapshot interval.
    /// Unverified entries are skipped: they carry only a relayed gossip
    /// claim, and persisting them would let junk records survive restarts.
    /// Gossip re-delivers any that are real. Records last seen longer ago than
    /// [`SNAPSHOT_STALE_SECS`] are pruned so the persisted table does not
    /// accumulate dead peers across restarts, unless the peer is still
    /// connected (a live connection is proof it is worth keeping).
    pub fn snapshot(&self) {
        let Some(ref store) = self.store else { return };
        let now = unix_timestamp_secs();
        let records: Vec<PeerSnapshot> = self
            .peers
            .iter()
            .filter(|r| {
                let entry = r.value();
                // A live connection proves reachability even when `last_seen`
                // (stamped at connect) has aged past the bound, so a peer held
                // open longer than the bound is kept rather than pruned.
                entry.is_verified()
                    && (entry.is_connected() || snapshot_is_fresh(now, entry.last_seen()))
            })
            .map(|r| PeerSnapshot::from(r.value().as_ref()))
            .collect();
        match store.store(&records) {
            Ok(()) => debug!(peers = records.len(), "wrote peer snapshot"),
            Err(e) => warn!(error = %e, "failed to write peer snapshot"),
        }
    }

    /// Decay every peer's score toward zero for the time elapsed since the
    /// peer's last decay pass.
    ///
    /// Disconnected peers decay at a 10 minute half-life, connected peers
    /// at double rate (5 minutes); both positive and negative scores decay,
    /// so reputation is recency-weighted. Elapsed time is tracked per peer
    /// ([`PeerEntry::decay_score`]), so the decay is exact even when a tick
    /// is missed. Banned peers are skipped: the unban path resets their
    /// score outright.
    fn decay_scores(&self, now_unix_secs: u64) {
        for r in self.peers.iter() {
            if let Some((old_score, new_score)) = r.value().decay_score(now_unix_secs) {
                self.score_distribution
                    .on_score_changed(old_score, new_score);
            }
        }
    }

    /// Lift every timed ban whose expiry has passed (`now >= until`).
    ///
    /// Permanent bans (expiry `None`) are never lifted here. Each expired
    /// ban goes through [`Self::unban`]: banned-set removal, score reset to
    /// the disconnect threshold, and a single
    /// [`PeerLifecycleEvent::Unbanned`](vertex_swarm_api::PeerLifecycleEvent)
    /// emission.
    fn expire_bans(&self, now_unix_secs: u64) {
        let expired: Vec<OverlayAddress> = self
            .banned_set
            .iter()
            .filter(|r| r.value().is_some_and(|until| now_unix_secs >= until))
            .map(|r| *r.key())
            .collect();

        for overlay in &expired {
            debug!(?overlay, "ban expired; unbanning peer");
            self.unban(overlay);
        }
    }

    /// Remove stale peers unconditionally.
    pub fn purge_stale(&self) {
        let stale: Vec<OverlayAddress> = self
            .peers
            .iter()
            .filter(|r| r.value().is_stale())
            .map(|r| *r.key())
            .collect();

        if stale.is_empty() {
            return;
        }

        for overlay in &stale {
            self.remove_peer(overlay);
        }

        debug!(
            removed = stale.len(),
            remaining = self.index.len(),
            "purged stale peers"
        );
    }

    /// Seed the peer set from the snapshot store.
    ///
    /// Called once during construction. Records are inserted freshest-first by
    /// `last_seen` so that when entries exceed the per-bin cap the freshest are
    /// kept and bias the dial queue; rediscovery via gossip refills any that
    /// were dropped and are still alive. Overlay breaks ties so restore order
    /// is deterministic regardless of how the store enumerates the table.
    pub(crate) fn load_from_store(&self) {
        let Some(ref store) = self.store else { return };

        let mut records = match store.load() {
            Ok(records) => records,
            Err(e) => {
                warn!(error = %e, "failed to load peer snapshot");
                return;
            }
        };
        records.sort_unstable_by_key(|r| (std::cmp::Reverse(r.last_seen), *r.peer.overlay()));

        let total = records.len();
        let mut loaded = 0usize;
        for snapshot in records {
            let overlay = OverlayAddress::from(*snapshot.peer.overlay());
            if self.index.add(overlay).is_err() {
                continue;
            }
            let entry = std::sync::Arc::new(PeerEntry::from_snapshot(
                snapshot,
                std::sync::Arc::clone(&self.scoring_config),
            ));
            self.score_distribution.on_peer_added(entry.score());
            on_health_added(entry.health_state());
            self.peers.insert(overlay, entry);
            loaded += 1;
        }

        gauge!("peer_manager_total_peers").set(self.index.len() as f64);
        // Restored entries start unverified until their next handshake.
        gauge!("peer_manager_unverified_peers").increment(loaded as f64);

        if total > 0 {
            info!(loaded, total, "loaded peer set from snapshot");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use vertex_net_peer_store::{MemoryPeerStore, PeerSnapshotStore};
    use vertex_swarm_primitives::SwarmNodeType;
    use vertex_swarm_test_utils::{
        MockIdentity, make_overlay, make_swarm_peer_minimal, test_overlay,
    };

    use super::*;
    use crate::PeerManagerConfig;

    /// Snapshot for a peer whose overlay is `byte` in its leading position;
    /// bytes with the top bit set all land in bin 0 relative to the all-zero
    /// local overlay, so they contend for the same per-bin cap.
    fn snap(byte: u8, last_seen: u64) -> PeerSnapshot {
        PeerSnapshot {
            peer: make_swarm_peer_minimal(byte),
            node_type: SwarmNodeType::Storer,
            last_seen,
        }
    }

    fn store_with(records: &[PeerSnapshot]) -> Arc<dyn PeerSnapshotStore<PeerSnapshot>> {
        let store: Arc<dyn PeerSnapshotStore<PeerSnapshot>> =
            Arc::new(MemoryPeerStore::<PeerSnapshot>::new());
        store.store(records).unwrap();
        store
    }

    /// Manager seeded from `store`, local overlay all-zero, given per-bin cap.
    fn seeded_manager(
        store: Arc<dyn PeerSnapshotStore<PeerSnapshot>>,
        max_per_bin: usize,
    ) -> Arc<PeerManager<MockIdentity>> {
        PeerManager::new(
            &MockIdentity::with_overlay(test_overlay(0)),
            PeerManagerConfig {
                store: Some(store),
                max_per_bin,
                ..Default::default()
            },
        )
    }

    #[test]
    fn over_cap_bin_keeps_the_freshest_records() {
        // Three peers in bin 0, cap 2, stored in an order the freshest do not
        // lead so the restore sort, not store iteration order, decides.
        let store = store_with(&[snap(0xE0, 10), snap(0x80, 30), snap(0xC0, 20)]);
        let pm = seeded_manager(store, 2);

        assert!(
            pm.swarm_peer(&make_overlay(0x80)).is_some(),
            "freshest record kept"
        );
        assert!(
            pm.swarm_peer(&make_overlay(0xC0)).is_some(),
            "next-freshest record kept"
        );
        assert!(
            pm.swarm_peer(&make_overlay(0xE0)).is_none(),
            "stalest record dropped when the bin is over cap"
        );
    }

    #[test]
    fn under_cap_restores_every_record() {
        // Same records, cap comfortably above the bin population: the sort
        // reorders insertion but nothing is dropped.
        let store = store_with(&[snap(0xE0, 10), snap(0x80, 30), snap(0xC0, 20)]);
        let pm = seeded_manager(store, 8);

        for byte in [0x80u8, 0xC0, 0xE0] {
            assert!(
                pm.swarm_peer(&make_overlay(byte)).is_some(),
                "under cap, every record is restored"
            );
        }
    }

    #[test]
    fn equal_last_seen_ties_break_on_overlay_deterministically() {
        // Two peers in bin 0 with identical last_seen, cap 1, stored so the
        // higher overlay leads. Without the overlay tiebreak the survivor
        // would depend on store enumeration order; with it the lower overlay
        // wins regardless, so the drop decision is deterministic.
        let store = store_with(&[snap(0xC0, 20), snap(0x80, 20)]);
        let pm = seeded_manager(store, 1);

        assert!(
            pm.swarm_peer(&make_overlay(0x80)).is_some(),
            "lower overlay wins the tie"
        );
        assert!(
            pm.swarm_peer(&make_overlay(0xC0)).is_none(),
            "higher overlay dropped when last_seen ties"
        );
    }

    #[test]
    fn snapshot_prune_bound_drops_only_stale() {
        let now = 100 * SNAPSHOT_STALE_SECS;
        assert!(snapshot_is_fresh(now, now), "seen now is fresh");
        assert!(
            snapshot_is_fresh(now, now - SNAPSHOT_STALE_SECS),
            "exactly at the bound is retained"
        );
        assert!(
            !snapshot_is_fresh(now, now - SNAPSHOT_STALE_SECS - 1),
            "one second past the bound is pruned"
        );
        assert!(
            snapshot_is_fresh(now, now + 5),
            "a last_seen ahead of now (clock skew) is retained"
        );
    }
}

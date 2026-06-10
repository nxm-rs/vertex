//! Periodic maintenance: stale-peer purging and snapshot persistence.

use metrics::gauge;
use std::sync::atomic::Ordering;
use tracing::{debug, info, warn};
use vertex_swarm_api::SwarmIdentity;
use vertex_swarm_primitives::OverlayAddress;

use crate::entry::{PeerEntry, PeerSnapshot, on_health_added};
use crate::manager::PeerManager;

impl<I: SwarmIdentity> PeerManager<I> {
    /// Single periodic entry point, driven from outside the crate (see
    /// [`crate::spawn_peer_manager_task`]).
    ///
    /// Purges stale never-connected peers and writes a snapshot when one is
    /// due ([`PeerManagerConfig::snapshot_interval`](crate::PeerManagerConfig)
    /// since the last write). `now_unix_secs` is injected so tests can drive
    /// the schedule without a clock.
    pub fn tick(&self, now_unix_secs: u64) {
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

    /// Write the full peer set to the snapshot store (no-op without one).
    ///
    /// Called by [`Self::tick`] on schedule and by topology on graceful
    /// shutdown so the final state is not lost to the snapshot interval.
    pub fn snapshot(&self) {
        let Some(ref store) = self.store else { return };
        let records: Vec<PeerSnapshot> = self
            .peers
            .iter()
            .map(|r| PeerSnapshot::from(r.value().as_ref()))
            .collect();
        match store.store(&records) {
            Ok(()) => debug!(peers = records.len(), "wrote peer snapshot"),
            Err(e) => warn!(error = %e, "failed to write peer snapshot"),
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
    /// Called once during construction. Entries that would exceed the
    /// per-bin cap are dropped; rediscovery via gossip refills them if they
    /// are still alive.
    pub(crate) fn load_from_store(&self) {
        let Some(ref store) = self.store else { return };

        let records = match store.load() {
            Ok(records) => records,
            Err(e) => {
                warn!(error = %e, "failed to load peer snapshot");
                return;
            }
        };

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

        if total > 0 {
            info!(loaded, total, "loaded peer set from snapshot");
        }
    }
}

//! Maintenance and persistence methods for hot/cold peer lifecycle.

use metrics::gauge;
use tracing::{debug, warn};
use vertex_net_peer_store::NetRecord;
use vertex_swarm_api::{SwarmIdentity, SwarmScoreStore};
use vertex_swarm_primitives::OverlayAddress;

use crate::entry::on_health_removed;
use crate::manager::PeerManager;

impl<I: SwarmIdentity> PeerManager<I> {
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

    /// Collect dirty hot peers into the write buffer for batched DB flush.
    pub fn collect_dirty(&self) {
        if self.store.is_none() {
            return;
        }
        for entry in self.peers.iter() {
            if entry.value().take_dirty() {
                self.buffer_entry(*entry.key(), entry.value());
            }
        }
    }

    /// Flush the write buffer to the DB (peer records and scores).
    pub fn flush_write_buffer(&self) {
        if let Some(ref store) = self.store
            && let Err(e) = self.write_buffer.flush(store.as_ref())
        {
            warn!(error = %e, "failed to flush write buffer");
        }
        if let Some(ref ss) = self.score_store {
            let scores = self.write_buffer.drain_scores();
            if !scores.is_empty()
                && let Err(e) = ss.save_score_batch(&scores)
            {
                warn!(error = %e, "failed to flush score buffer");
            }
        }
    }

    /// Evict non-connected peers from the hot cache to keep it bounded.
    ///
    /// Peers with consecutive failures > 0 are considered disconnected and
    /// eligible for eviction. Their state is saved to DB before removal.
    pub fn evict_cold(&self) {
        if self.store.is_none() {
            return;
        }
        let current = self.peers.len();
        if current <= self.max_hot_peers {
            return;
        }

        let to_evict = current.saturating_sub(self.max_hot_peers);

        // Collect eviction candidates: peers with failures (not connected)
        let mut candidates: Vec<(OverlayAddress, u64)> = self
            .peers
            .iter()
            .filter(|r| r.value().consecutive_failures() > 0)
            .map(|r| (*r.key(), r.value().last_seen()))
            .collect();

        // Sort by last_seen ascending (oldest first)
        candidates.sort_unstable_by_key(|&(_, last_seen)| last_seen);

        let mut evicted = 0;
        for (overlay, _) in candidates.into_iter().take(to_evict) {
            // Remove from hot cache and snapshot to DB in one lookup
            if let Some((_, entry)) = self.peers.remove(&overlay) {
                self.buffer_entry(overlay, &entry);
                self.score_distribution.on_peer_removed(entry.score());
                on_health_removed(entry.health_state());
            }
            evicted += 1;
        }

        if evicted > 0 {
            self.flush_write_buffer();
            gauge!("peer_manager_hot_peers").set(self.peers.len() as f64);
            debug!(
                evicted,
                remaining_hot = self.peers.len(),
                "evicted cold peers from hot cache"
            );
        }
    }

    /// Replenish depleted proximity bins from the database.
    ///
    /// Scans bins below half capacity (low-water mark), then streams DB keys
    /// to fill them. Runs periodically from the persistence task.
    pub fn replenish_bins(&self) {
        let Some(ref store) = self.store else { return };

        let max_per_bin = self.index.max_per_bin();
        if max_per_bin == 0 {
            return; // Unbounded index, nothing to replenish
        }

        // Build per-bin remaining capacity array for O(1) lookup.
        let low_water = max_per_bin / 2;
        let bin_sizes = self.index.bin_sizes();
        let max_po = self.index.max_po() as usize;
        let mut remaining = vec![0usize; max_po + 1];
        let mut any_depleted = false;
        for (po, &size) in bin_sizes.iter().enumerate() {
            if size < low_water {
                remaining[po] = max_per_bin.saturating_sub(size);
                any_depleted = true;
            }
        }
        if !any_depleted {
            return;
        }

        // Key-only scan: loads overlay addresses without deserializing values.
        let overlays = match store.load_ids() {
            Ok(ids) => ids,
            Err(e) => {
                warn!(error = %e, "failed to load peer IDs for bin replenishment");
                return;
            }
        };

        let mut added = 0usize;
        for overlay in &overlays {
            if self.index.exists(overlay) {
                continue;
            }
            let po = self.index.bin_for(overlay) as usize;
            if remaining[po] > 0 && self.index.add(*overlay).is_ok() {
                added += 1;
                remaining[po] -= 1;
            }
        }

        if added > 0 {
            gauge!("peer_manager_total_peers").set(self.index.len() as f64);
            debug!(added, "replenished depleted proximity bins from store");
        }
    }

    /// Load the overlay index and banned set from the store.
    ///
    /// Uses key-only scan for the overlay index (no value deserialization),
    /// then loads banned overlays separately.
    /// Called once during construction. Does NOT populate the DashMap;
    /// peers are loaded on demand via `get_or_load`.
    pub(crate) fn load_index_from_store(&self) {
        let Some(ref store) = self.store else { return };

        // Phase 1: Key-only scan for overlay index (no value deserialization).
        let overlays = match store.load_ids() {
            Ok(ids) => ids,
            Err(e) => {
                warn!(error = %e, "failed to load peer IDs from store");
                return;
            }
        };

        let total_stored = overlays.len();
        let mut indexed = 0;
        for overlay in &overlays {
            if self.index.add(*overlay).is_ok() {
                indexed += 1;
            }
        }

        // Phase 2: Load banned overlays (needs value deserialization for ban_info).
        let mut banned = 0;
        if let Some(ref ss) = self.score_store {
            match ss.load_banned_overlays() {
                Ok(banned_overlays) => {
                    for overlay in &banned_overlays {
                        self.banned_set.insert(*overlay);
                    }
                    banned = banned_overlays.len();
                }
                Err(e) => warn!(error = %e, "failed to load banned peers"),
            }
        } else {
            // No score store — fall back to full record scan for ban info.
            match store.load_all() {
                Ok(records) => {
                    for record in &records {
                        if record.is_banned() {
                            self.banned_set.insert(*record.id());
                            banned += 1;
                        }
                    }
                }
                Err(e) => warn!(error = %e, "failed to load ban info from store"),
            }
        }

        gauge!("peer_manager_total_peers").set(indexed as f64);
        gauge!("peer_manager_banned_peers").set(banned as f64);
        gauge!("peer_manager_hot_peers").set(0.0f64);
        gauge!("peer_manager_stored_peers").set(total_stored as f64);

        if total_stored > 0 {
            debug!(
                total_stored,
                indexed, banned, "loaded peer index from store"
            );
        }
    }
}

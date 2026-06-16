//! IP association tracking for identity-cycling detection.
//!
//! Maps the remote IP of each handshake-completed connection to the set of
//! overlay addresses recently seen from it. An attacker that keeps one IP
//! but rotates nonces (and therefore overlays) shows up as a stream of new
//! overlays from a single IP; a legitimate NAT cohort shows up as a bounded,
//! stable set. The tracker only observes handshake completions, so stable
//! long-lived connections contribute one sighting per connection rather
//! than one per window, and a steady cohort behind one IPv4 address ages
//! out of the window instead of accumulating.
//!
//! Grouping: IPv4 is tracked per exact address. IPv6 is tracked per /64
//! prefix because a single end site receives a /64 under the standard
//! allocation policy; tracking exact IPv6 addresses would let one host
//! rotate through 2^64 interface identifiers to evade the cap.
//!
//! Hot paths are O(1): sightings live in a per-group bounded [`VecDeque`]
//! (`push_back`/`pop_front` only) and a reverse index maps each overlay to
//! the groups it was seen from, so overlay removal never scans a deque.
//! Entries removed through the reverse index leave tombstones in the deque
//! that are dropped when they reach the front; a per-group live map (one
//! sighting id per overlay) keeps the distinct-overlay count exact without
//! rescanning, even when a removed overlay is re-recorded while its old
//! entry is still queued.
//!
//! Consumers: [`PeerManager::on_peer_connected`] feeds the tracker and
//! reports cap crossings through the single scoring path; the inbound
//! handshake rate limiter consults the per-IP counts through
//! [`PeerManager::overlays_seen_from_ip`] before spending signature
//! recovery on a suspect source.
//!
//! [`PeerManager::on_peer_connected`]: crate::PeerManager::on_peer_connected
//! [`PeerManager::overlays_seen_from_ip`]: crate::PeerManager::overlays_seen_from_ip

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::time::Duration;

use vertex_swarm_primitives::OverlayAddress;

/// Abuse-tracking key derived from a remote IP address.
///
/// IPv4 addresses are kept exact: one IPv4 address is one routable
/// endpoint, even when shared by a NAT cohort. IPv6 addresses are
/// collapsed to their /64 prefix, the standard end-site allocation, so a
/// single host cannot evade tracking by rotating interface identifiers
/// within its prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct IpGroup(IpAddr);

impl From<IpAddr> for IpGroup {
    fn from(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(_) => Self(ip),
            // IPv4-mapped IPv6 (::ffff:a.b.c.d, common on dual-stack
            // listeners) is canonicalized to the exact IPv4 address;
            // masking it to /64 would collapse every mapped source into
            // one group.
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => Self(IpAddr::V4(v4)),
                None => Self(IpAddr::V6((u128::from(v6) & !((1u128 << 64) - 1)).into())),
            },
        }
    }
}

/// Configuration for [`IpTracker`].
#[derive(Debug, Clone)]
pub struct IpTrackerConfig {
    /// Distinct overlays tolerated per IP group within [`Self::window`].
    ///
    /// Sightings beyond this count flag identity cycling. The cap bounds
    /// the rate of NEW overlays from one IP, not the number of concurrent
    /// peers: a stable cohort behind one NAT is recorded once per
    /// handshake and ages out of the window, while a cycling attacker
    /// keeps completing handshakes under fresh overlays and accumulates.
    pub max_overlays_per_ip: usize,
    /// Sliding window over which sightings count toward the cap.
    pub window: Duration,
    /// Hard bound on sightings retained per IP group.
    ///
    /// Caps tracker memory against an attacker that floods one IP with
    /// new overlays inside a single window; the oldest sighting is
    /// evicted when the bound is reached.
    pub max_sightings_per_ip: usize,
    /// Hard cap on live concurrent connections per IP group (`None` =
    /// unlimited, the default); trusted/local-subnet peers exempt.
    pub max_connections_per_ip: Option<usize>,
}

impl IpTrackerConfig {
    /// Default distinct-overlay cap per IP group (128); sized for high-density IPs.
    pub const DEFAULT_MAX_OVERLAYS_PER_IP: usize = 128;

    /// Default sighting window (15 minutes).
    ///
    /// Long enough that an attacker cannot simply pace identity rotation
    /// below notice, short enough that a reconnect burst after an outage
    /// (a NAT cohort redialing at once) clears quickly.
    pub const DEFAULT_WINDOW: Duration = Duration::from_secs(15 * 60);

    /// Default per-IP sighting bound (512, 4x the overlay cap).
    pub const DEFAULT_MAX_SIGHTINGS_PER_IP: usize = 512;

    /// Default live per-IP connection cap: disabled (`None`).
    pub const DEFAULT_MAX_CONNECTIONS_PER_IP: Option<usize> = None;
}

impl Default for IpTrackerConfig {
    fn default() -> Self {
        Self {
            max_overlays_per_ip: Self::DEFAULT_MAX_OVERLAYS_PER_IP,
            window: Self::DEFAULT_WINDOW,
            max_sightings_per_ip: Self::DEFAULT_MAX_SIGHTINGS_PER_IP,
            max_connections_per_ip: Self::DEFAULT_MAX_CONNECTIONS_PER_IP,
        }
    }
}

/// Result of recording one overlay sighting from an IP group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordOutcome {
    /// First sighting of this overlay from this IP group within the window.
    Recorded,
    /// The overlay is already associated with this IP group; no change.
    AlreadyTracked,
    /// The sighting was recorded and pushed the distinct-overlay count
    /// past the cap: the overlay is a NEW identity from an IP that has
    /// already shown more identities than the window tolerates.
    CyclingDetected {
        /// Distinct overlays now associated with the IP group.
        distinct: usize,
    },
}

/// One recorded sighting in a group's deque.
#[derive(Debug, Clone, Copy)]
struct Sighting {
    overlay: OverlayAddress,
    /// Unix seconds at which the sighting was recorded.
    seen_at: u64,
    /// Group-local id distinguishing this sighting from earlier tombstoned
    /// sightings of the same overlay (an overlay removed and re-recorded
    /// while its old entry is still queued).
    id: u64,
}

/// Recent sightings for one IP group.
#[derive(Debug, Default)]
struct GroupSightings {
    /// Sightings, oldest at the front. May contain tombstones: entries no
    /// longer present in `live` that are dropped lazily when they reach
    /// the front.
    sightings: VecDeque<Sighting>,
    /// The single live sighting id per overlay; `live.len()` is the
    /// distinct-overlay count. A deque entry is live iff its id matches.
    live: HashMap<OverlayAddress, u64>,
    /// Next sighting id for this group.
    next_id: u64,
}

/// Bounded map of remote IP groups to the overlays recently seen from them.
///
/// Time never comes from a clock here: every operation takes `now` in unix
/// seconds from the caller, so the tracker is deterministic under test and
/// free of platform time dependencies.
#[derive(Debug)]
pub(crate) struct IpTracker {
    config: IpTrackerConfig,
    /// Sightings per IP group.
    groups: HashMap<IpGroup, GroupSightings>,
    /// Reverse index for O(1) overlay removal: which groups currently hold
    /// a live sighting of this overlay.
    by_overlay: HashMap<OverlayAddress, HashSet<IpGroup>>,
}

impl IpTracker {
    pub(crate) fn new(config: IpTrackerConfig) -> Self {
        Self {
            config,
            groups: HashMap::new(),
            by_overlay: HashMap::new(),
        }
    }

    pub(crate) fn config(&self) -> &IpTrackerConfig {
        &self.config
    }

    /// Number of IP groups currently holding at least one sighting.
    pub(crate) fn tracked_ips(&self) -> usize {
        self.groups.len()
    }

    /// Record a handshake-completed sighting of `overlay` from `group`.
    ///
    /// O(1) amortized: pruning pops expired or tombstoned entries from the
    /// deque front, the dedupe check is one live-map lookup, and the
    /// insert is a `push_back`.
    pub(crate) fn record(
        &mut self,
        overlay: OverlayAddress,
        group: IpGroup,
        now: u64,
    ) -> RecordOutcome {
        self.prune_group(group, now);

        let entry = self.groups.entry(group).or_default();
        if entry.live.contains_key(&overlay) {
            return RecordOutcome::AlreadyTracked;
        }

        if entry.sightings.len() >= self.config.max_sightings_per_ip {
            drop_front(entry, &mut self.by_overlay, group);
        }
        let id = entry.next_id;
        entry.next_id += 1;
        entry.sightings.push_back(Sighting {
            overlay,
            seen_at: now,
            id,
        });
        entry.live.insert(overlay, id);
        let distinct = entry.live.len();
        self.by_overlay.entry(overlay).or_default().insert(group);

        if distinct > self.config.max_overlays_per_ip {
            RecordOutcome::CyclingDetected { distinct }
        } else {
            RecordOutcome::Recorded
        }
    }

    /// Distinct overlays seen from the group containing `ip` within the
    /// window ending at `now`.
    pub(crate) fn distinct_overlays(&mut self, ip: IpAddr, now: u64) -> usize {
        let group = IpGroup::from(ip);
        self.prune_group(group, now);
        self.groups.get(&group).map_or(0, |entry| entry.live.len())
    }

    /// Drop every live sighting of `overlay` across all IP groups.
    ///
    /// O(1) in deque terms: the reverse index names the affected groups
    /// and their live maps drop the overlay; the deque entries become
    /// tombstones that fall off the front later. A group whose last live
    /// sighting is removed is dropped entirely.
    pub(crate) fn on_overlay_removed(&mut self, overlay: &OverlayAddress) {
        let Some(groups) = self.by_overlay.remove(overlay) else {
            return;
        };
        for group in groups {
            if let Some(entry) = self.groups.get_mut(&group) {
                entry.live.remove(overlay);
                if entry.live.is_empty() {
                    // Every remaining deque entry is a tombstone.
                    self.groups.remove(&group);
                }
            }
        }
    }

    /// Pop expired and tombstoned entries from the front of `group`'s
    /// deque. Each popped entry is O(1); the amortized cost is one pop per
    /// recorded sighting over the tracker's lifetime.
    fn prune_group(&mut self, group: IpGroup, now: u64) {
        let Some(entry) = self.groups.get_mut(&group) else {
            return;
        };
        let window_secs = self.config.window.as_secs();
        while let Some(front) = entry.sightings.front() {
            let is_live = entry.live.get(&front.overlay) == Some(&front.id);
            if is_live && now.saturating_sub(front.seen_at) <= window_secs {
                break;
            }
            drop_front(entry, &mut self.by_overlay, group);
        }
        if entry.sightings.is_empty() {
            self.groups.remove(&group);
        }
    }
}

/// Pop the oldest sighting of `group`, maintaining the live map and
/// reverse index. Tombstones (entries whose id no longer matches the
/// overlay's live sighting) are dropped without touching either.
fn drop_front(
    entry: &mut GroupSightings,
    by_overlay: &mut HashMap<OverlayAddress, HashSet<IpGroup>>,
    group: IpGroup,
) {
    let Some(sighting) = entry.sightings.pop_front() else {
        return;
    };
    if entry.live.get(&sighting.overlay) == Some(&sighting.id) {
        entry.live.remove(&sighting.overlay);
        remove_reverse_entry(by_overlay, &sighting.overlay, &group);
    }
}

/// Remove one `(overlay, group)` association from the reverse index,
/// dropping the overlay's set when it empties.
fn remove_reverse_entry(
    by_overlay: &mut HashMap<OverlayAddress, HashSet<IpGroup>>,
    overlay: &OverlayAddress,
    group: &IpGroup,
) {
    if let Some(groups) = by_overlay.get_mut(overlay) {
        groups.remove(group);
        if groups.is_empty() {
            by_overlay.remove(overlay);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_test_utils::test_overlay;

    fn tracker(cap: usize) -> IpTracker {
        IpTracker::new(IpTrackerConfig {
            max_overlays_per_ip: cap,
            window: Duration::from_secs(900),
            max_sightings_per_ip: cap * 4,
            ..Default::default()
        })
    }

    fn v4(last: u8) -> IpGroup {
        IpGroup::from(IpAddr::from([203, 0, 113, last]))
    }

    #[test]
    fn detection_at_cap_boundary() {
        let mut t = tracker(3);
        let ip = v4(1);

        // Exactly cap overlays: all recorded, none flagged.
        for n in 1..=3 {
            assert_eq!(t.record(test_overlay(n), ip, 100), RecordOutcome::Recorded);
        }
        // The cap+1th distinct overlay crosses the boundary.
        assert_eq!(
            t.record(test_overlay(4), ip, 100),
            RecordOutcome::CyclingDetected { distinct: 4 }
        );
        // Every further NEW overlay stays flagged while over the cap.
        assert_eq!(
            t.record(test_overlay(5), ip, 100),
            RecordOutcome::CyclingDetected { distinct: 5 }
        );
    }

    #[test]
    fn reconnect_of_known_overlay_is_not_a_new_sighting() {
        let mut t = tracker(3);
        let ip = v4(1);

        for n in 1..=3 {
            t.record(test_overlay(n), ip, 100);
        }
        // The same overlays reconnecting must not push past the cap.
        for n in 1..=3 {
            assert_eq!(
                t.record(test_overlay(n), ip, 200),
                RecordOutcome::AlreadyTracked
            );
        }
        assert_eq!(t.distinct_overlays(IpAddr::from([203, 0, 113, 1]), 200), 3);
    }

    #[test]
    fn window_expiry_frees_the_cap() {
        let mut t = tracker(3);
        let ip = v4(1);

        for n in 1..=3 {
            t.record(test_overlay(n), ip, 100);
        }
        // Just past the window the old sightings expire and new overlays
        // are clean again.
        let later = 100 + 901;
        assert_eq!(
            t.record(test_overlay(4), ip, later),
            RecordOutcome::Recorded
        );
        assert_eq!(
            t.distinct_overlays(IpAddr::from([203, 0, 113, 1]), later),
            1
        );
    }

    #[test]
    fn distinct_ips_do_not_interact() {
        let mut t = tracker(2);
        for n in 1..=2 {
            assert_eq!(
                t.record(test_overlay(n), v4(1), 100),
                RecordOutcome::Recorded
            );
        }
        // A different IPv4 address is a separate group.
        assert_eq!(
            t.record(test_overlay(3), v4(2), 100),
            RecordOutcome::Recorded
        );
        assert_eq!(t.tracked_ips(), 2);
    }

    #[test]
    fn ipv6_same_slash64_is_one_group() {
        let a: IpAddr = "2001:db8:1:1::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:1:ffff:ffff:ffff:ffff".parse().unwrap();
        let c: IpAddr = "2001:db8:1:2::1".parse().unwrap();

        assert_eq!(IpGroup::from(a), IpGroup::from(b));
        assert_ne!(IpGroup::from(a), IpGroup::from(c));

        let mut t = tracker(1);
        assert_eq!(
            t.record(test_overlay(1), IpGroup::from(a), 100),
            RecordOutcome::Recorded
        );
        // A second overlay from another address in the same /64 counts
        // against the same group.
        assert_eq!(
            t.record(test_overlay(2), IpGroup::from(b), 100),
            RecordOutcome::CyclingDetected { distinct: 2 }
        );
        // A different /64 is a separate group.
        assert_eq!(
            t.record(test_overlay(3), IpGroup::from(c), 100),
            RecordOutcome::Recorded
        );
    }

    #[test]
    fn overlay_removal_cleans_reverse_index_and_counts() {
        let mut t = tracker(3);
        let ip = v4(1);

        for n in 1..=3 {
            t.record(test_overlay(n), ip, 100);
        }
        t.on_overlay_removed(&test_overlay(2));
        assert_eq!(t.distinct_overlays(IpAddr::from([203, 0, 113, 1]), 100), 2);
        // The freed slot keeps a new overlay under the cap.
        assert_eq!(t.record(test_overlay(4), ip, 100), RecordOutcome::Recorded);
        // The removed overlay may legitimately return later.
        assert_eq!(
            t.record(test_overlay(2), ip, 100),
            RecordOutcome::CyclingDetected { distinct: 4 }
        );
    }

    #[test]
    fn removing_last_overlay_drops_the_group() {
        let mut t = tracker(3);
        t.record(test_overlay(1), v4(1), 100);
        assert_eq!(t.tracked_ips(), 1);

        t.on_overlay_removed(&test_overlay(1));
        assert_eq!(t.tracked_ips(), 0);
        assert_eq!(t.distinct_overlays(IpAddr::from([203, 0, 113, 1]), 100), 0);
    }

    #[test]
    fn removal_affects_every_group_the_overlay_was_seen_from() {
        let mut t = tracker(3);
        t.record(test_overlay(1), v4(1), 100);
        t.record(test_overlay(1), v4(2), 100);
        t.record(test_overlay(2), v4(2), 100);

        t.on_overlay_removed(&test_overlay(1));
        assert_eq!(t.distinct_overlays(IpAddr::from([203, 0, 113, 1]), 100), 0);
        assert_eq!(t.distinct_overlays(IpAddr::from([203, 0, 113, 2]), 100), 1);
    }

    #[test]
    fn sighting_bound_evicts_oldest() {
        let mut t = IpTracker::new(IpTrackerConfig {
            max_overlays_per_ip: 2,
            window: Duration::from_secs(900),
            max_sightings_per_ip: 3,
            ..Default::default()
        });
        let ip = v4(1);
        for n in 1..=3 {
            t.record(test_overlay(n), ip, 100);
        }
        // The 4th sighting evicts overlay 1 (oldest) to hold the bound.
        t.record(test_overlay(4), ip, 100);
        assert_eq!(t.distinct_overlays(IpAddr::from([203, 0, 113, 1]), 100), 3);
        // Overlay 1 is no longer associated, so re-recording it counts as new.
        assert_eq!(
            t.record(test_overlay(1), ip, 100),
            RecordOutcome::CyclingDetected { distinct: 3 }
        );
    }

    #[test]
    fn ipv4_mapped_ipv6_groups_as_exact_ipv4() {
        let mapped: IpAddr = "::ffff:203.0.113.7".parse().unwrap();
        let plain: IpAddr = "203.0.113.7".parse().unwrap();
        let other_mapped: IpAddr = "::ffff:203.0.113.8".parse().unwrap();

        assert_eq!(IpGroup::from(mapped), IpGroup::from(plain));
        assert_ne!(IpGroup::from(mapped), IpGroup::from(other_mapped));
    }

    #[test]
    fn tombstone_does_not_corrupt_rerecorded_overlay() {
        let mut t = tracker(3);
        let ip = v4(1);

        t.record(test_overlay(1), ip, 0);
        t.record(test_overlay(2), ip, 0);
        // Overlay 1 is removed (tombstone stays queued) and then returns.
        t.on_overlay_removed(&test_overlay(1));
        t.record(test_overlay(1), ip, 10);

        // Pruning past the old entries' expiry must drop the tombstone and
        // overlay 2's sighting, but keep overlay 1's fresh re-record.
        assert_eq!(
            t.distinct_overlays(IpAddr::from([203, 0, 113, 1]), 901),
            1,
            "the re-recorded sighting must survive its tombstone's expiry"
        );
        assert_eq!(
            t.record(test_overlay(1), ip, 901),
            RecordOutcome::AlreadyTracked
        );
    }

    #[test]
    fn expiry_drops_empty_group() {
        let mut t = tracker(3);
        t.record(test_overlay(1), v4(1), 100);
        assert_eq!(t.tracked_ips(), 1);
        assert_eq!(
            t.distinct_overlays(IpAddr::from([203, 0, 113, 1]), 100 + 901),
            0
        );
        assert_eq!(t.tracked_ips(), 0);
    }
}

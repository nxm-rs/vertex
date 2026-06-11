//! Tuning knobs for the gossip subsystem.

use std::time::Duration;

/// Tuning knobs for gossip peer exchange and record intake.
///
/// None of these values are fixed by the Swarm protocol; they trade
/// responsiveness against memory and abuse resistance.
/// [`GossipConfig::default`] is the production tuning. Tests tighten
/// individual fields with struct-update syntax for deterministic timing,
/// for example `GossipConfig { max_records_per_gossiper: 4, ..Default::default() }`.
///
/// See the `gossip` module docs for how the limits relate to each other.
#[derive(Debug, Clone)]
pub struct GossipConfig {
    /// Interval between neighborhood refresh broadcasts to connected
    /// neighbors.
    ///
    /// Also bounds memory indirectly: last-broadcast timestamps older than
    /// twice this interval are evicted, so the broadcast bookkeeping never
    /// outlives two refresh cycles. Shorter intervals push redundant peer
    /// lists onto stable neighbors; longer intervals slow discovery of
    /// neighborhood changes.
    pub refresh_interval: Duration,

    /// Delay before exchanging gossip with a peer we dialed for gossip
    /// purposes.
    ///
    /// A freshly dialed peer may drop the connection immediately if its bin
    /// is saturated; waiting avoids wasting an exchange on a connection that
    /// is about to close. Exchanges scheduled behind this delay are cancelled
    /// if the connection closes first.
    pub health_check_delay: Duration,

    /// Minimum time between processing two records for the same overlay
    /// whose multiaddrs have not changed.
    ///
    /// Peers may re-sign and re-broadcast their record on every gossip
    /// round, so the same overlay arrives repeatedly with a fresh signature
    /// but identical addresses. Such records carry no news; the cooldown
    /// drops them before they reach the peer manager. A record whose
    /// multiaddrs differ from the last processed one bypasses the cooldown
    /// (it carries real news). The interval also serves as the window for
    /// the per-gossiper admission budget.
    pub record_cooldown: Duration,

    /// Maximum records admitted from a single gossiper per
    /// [`Self::record_cooldown`] window.
    ///
    /// Per-source rate limit: one malicious or buggy gossiper cannot flood
    /// the known table with junk records by itself. Records suppressed by
    /// the cooldown or skipped as already known do not count against the
    /// budget.
    pub max_records_per_gossiper: usize,

    /// LRU capacity of the per-gossiper admission counters.
    ///
    /// Memory bound on rate-limit bookkeeping. Must be at least the
    /// expected number of concurrently gossiping peers, otherwise counter
    /// eviction lets a gossiper restart its `max_records_per_gossiper`
    /// budget early.
    pub max_tracked_gossipers: usize,

    /// LRU capacity of the per-overlay cooldown cache.
    ///
    /// Memory bound on cooldown bookkeeping; each entry is an overlay, a
    /// timestamp, and a multiaddr fingerprint. Sized well above the known
    /// table so re-broadcast suppression holds across the whole supply.
    pub max_tracked_cooldowns: usize,
}

impl Default for GossipConfig {
    fn default() -> Self {
        Self {
            refresh_interval: Duration::from_secs(600),
            health_check_delay: Duration::from_millis(500),
            record_cooldown: Duration::from_secs(300),
            max_records_per_gossiper: 64,
            max_tracked_gossipers: 1024,
            max_tracked_cooldowns: 8192,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults are the production tuning; this pins them so a default
    /// change is a deliberate diff, not an accident.
    #[test]
    fn defaults_match_production_tuning() {
        let config = GossipConfig::default();
        assert_eq!(config.refresh_interval, Duration::from_secs(600));
        assert_eq!(config.health_check_delay, Duration::from_millis(500));
        assert_eq!(config.record_cooldown, Duration::from_secs(300));
        assert_eq!(config.max_records_per_gossiper, 64);
        assert_eq!(config.max_tracked_gossipers, 1024);
        assert_eq!(config.max_tracked_cooldowns, 8192);
    }
}

//! Tuning knobs for the gossip subsystem.

use std::time::Duration;

/// Tuning knobs for gossip peer exchange and verification.
///
/// None of these values are fixed by the Swarm protocol; they trade
/// responsiveness against memory, dial load, and abuse resistance.
/// [`GossipConfig::default`] is the production tuning. Tests tighten
/// individual fields with struct-update syntax for deterministic timing,
/// for example `GossipConfig { max_total_pending: 8, ..Default::default() }`.
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

    /// Idle connection timeout for the ephemeral verification swarm.
    ///
    /// Verification connections are short-lived by design: handshake, then
    /// disconnect. This timeout reclaims connections that stall after the
    /// transport opens, bounding the file descriptors the verifier can hold.
    pub verification_idle_timeout: Duration,

    /// Maximum unverified peers a single gossiper may have queued for
    /// verification at once.
    ///
    /// Per-source rate limit: one malicious or buggy gossiper cannot fill
    /// the global verification queue by itself. With the default global cap
    /// (`max_total_pending` 1024) and this per-source cap (64), it takes at
    /// least 16 distinct gossipers at their limit to saturate the queue.
    pub max_pending_per_gossiper: usize,

    /// Maximum unverified peers queued for verification across all
    /// gossipers.
    ///
    /// Global memory bound on the verification queue: each entry holds a
    /// full unverified peer record. The default (1024) is sized for roughly
    /// 32 simultaneously gossiping connections sending a typical gossip
    /// message of about 30 peers each. Note this is deliberately smaller
    /// than `max_concurrent_verifications * max_pending_per_gossiper`
    /// (32 * 64 = 2048): the global cap binds first under broad load, the
    /// per-gossiper cap binds first under a single abusive source.
    pub max_total_pending: usize,

    /// Time a queued or in-flight verification may live before it expires.
    ///
    /// Expiry frees queue slots held by peers that never become dialable,
    /// so a burst of bad gossip cannot pin the queue at `max_total_pending`
    /// forever. Also serves as the in-flight dial timeout.
    pub pending_expiry: Duration,

    /// Maximum concurrent verification dials.
    ///
    /// Bounds the outbound connection load the verifier adds on top of
    /// regular topology dialing. Queued entries beyond this drain as
    /// in-flight verifications resolve.
    pub max_concurrent_verifications: usize,

    /// LRU capacity of the per-gossiper pending counters.
    ///
    /// Memory bound on rate-limit bookkeeping. Must be at least the
    /// expected number of concurrently gossiping peers, otherwise counter
    /// eviction lets a gossiper restart its `max_pending_per_gossiper`
    /// budget early.
    pub max_tracked_gossipers: usize,

    /// Interval between cleanup passes that evict expired queue entries.
    ///
    /// Sets how stale the queue can get between passes; it only needs to be
    /// comfortably below `pending_expiry` to keep expiry timely.
    pub cleanup_interval: Duration,

    /// LRU capacity of the backoff cache for unreachable gossiped peers.
    ///
    /// Memory bound on backoff bookkeeping. Sized to match
    /// `max_total_pending` so every queued peer that fails can be tracked.
    pub backoff_capacity: usize,

    /// Backoff applied after the first failed verification dial of a peer.
    ///
    /// Grows exponentially per consecutive failure, capped at
    /// `backoff_max`. Stops the verifier from redialing a dead address every
    /// time a gossiper repeats it.
    pub backoff_base: Duration,

    /// Cap on the exponential backoff growth.
    pub backoff_max: Duration,

    /// LRU capacity of the ban cache for repeatedly unreachable peers.
    ///
    /// Memory bound on ban bookkeeping. Larger than `backoff_capacity`
    /// because bans live much longer (`ban_ttl`) than backoff entries.
    pub ban_capacity: usize,

    /// Consecutive failed verification dials before a peer is banned from
    /// re-verification.
    pub ban_after_failures: u32,

    /// Time a ban lasts before the peer may be verified again.
    ///
    /// A successful verification clears backoff and ban state early.
    pub ban_ttl: Duration,
}

impl Default for GossipConfig {
    fn default() -> Self {
        Self {
            refresh_interval: Duration::from_secs(600),
            health_check_delay: Duration::from_millis(500),
            verification_idle_timeout: Duration::from_secs(10),
            max_pending_per_gossiper: 64,
            max_total_pending: 1024,
            pending_expiry: Duration::from_secs(60),
            max_concurrent_verifications: 32,
            max_tracked_gossipers: 1024,
            cleanup_interval: Duration::from_secs(10),
            backoff_capacity: 1024,
            backoff_base: Duration::from_secs(5),
            backoff_max: Duration::from_secs(20),
            ban_capacity: 8192,
            ban_after_failures: 3,
            ban_ttl: Duration::from_secs(3600),
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
        assert_eq!(config.verification_idle_timeout, Duration::from_secs(10));
        assert_eq!(config.max_pending_per_gossiper, 64);
        assert_eq!(config.max_total_pending, 1024);
        assert_eq!(config.pending_expiry, Duration::from_secs(60));
        assert_eq!(config.max_concurrent_verifications, 32);
        assert_eq!(config.max_tracked_gossipers, 1024);
        assert_eq!(config.cleanup_interval, Duration::from_secs(10));
        assert_eq!(config.backoff_capacity, 1024);
        assert_eq!(config.backoff_base, Duration::from_secs(5));
        assert_eq!(config.backoff_max, Duration::from_secs(20));
        assert_eq!(config.ban_capacity, 8192);
        assert_eq!(config.ban_after_failures, 3);
        assert_eq!(config.ban_ttl, Duration::from_secs(3600));
    }
}

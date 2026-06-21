//! Pullsync behaviour counters. Label-free; per-peer detail lives in scoring and
//! the structured debug log.

use metrics::counter;

/// An inbound cursor handshake was answered.
pub fn inbound_cursors_served() {
    counter!("swarm.pullsync.inbound_cursors_served_total").increment(1);
}

/// An inbound range request was served, with `delivered` chunks sent.
pub fn inbound_range_served(delivered: u64) {
    counter!("swarm.pullsync.inbound_ranges_served_total").increment(1);
    counter!("swarm.pullsync.inbound_chunks_delivered_total").increment(delivered);
}

/// An inbound substream was refused because the per-peer rate limit was hit.
pub fn inbound_rate_limited() {
    counter!("swarm.pullsync.inbound_rate_limited_total").increment(1);
}

/// An inbound serving future failed before completing the exchange.
pub fn inbound_failed() {
    counter!("swarm.pullsync.inbound_failed_total").increment(1);
}

/// An outbound cursor handshake completed.
pub fn outbound_cursors_received() {
    counter!("swarm.pullsync.outbound_cursors_received_total").increment(1);
}

/// An outbound range exchange delivered `count` chunks.
pub fn outbound_range_delivered(count: u64) {
    counter!("swarm.pullsync.outbound_ranges_delivered_total").increment(1);
    counter!("swarm.pullsync.outbound_chunks_received_total").increment(count);
}

/// An outbound command failed.
pub fn outbound_failed() {
    counter!("swarm.pullsync.outbound_failed_total").increment(1);
}

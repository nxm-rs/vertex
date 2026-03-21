//! Metrics for bandwidth accounting.
//!
//! All balance and settlement values are in **Accounting Units (AU)**, not bytes.
//! AU encodes network cost based on Kademlia proximity:
//! `price = (max_po - proximity + 1) * base_price`.

use vertex_metrics::labels::direction;

/// Record a chunk transfer: one chunk counted, plus its AU cost.
///
/// Called on every `record()` in [`AccountingPeerHandle`](crate::AccountingPeerHandle).
pub fn record_chunk_transfer(amount_au: u64, upload: bool) {
    let dir = if upload { direction::INBOUND } else { direction::OUTBOUND };
    metrics::counter!("accounting_chunks_total", "direction" => dir)
        .increment(1);
    metrics::counter!("accounting_au_total", "direction" => dir)
        .increment(amount_au);
}

/// Record a disconnect limit violation (peer rejected).
pub fn record_disconnect_violation() {
    metrics::counter!("accounting_disconnect_violations_total").increment(1);
}

/// Record that a settlement was attempted.
pub fn record_settlement_attempt(provider: &'static str) {
    metrics::counter!("accounting_settlement_attempts_total", "provider" => provider)
        .increment(1);
}

/// Record that a settlement completed successfully (amount in AU).
pub fn record_settlement_success(provider: &'static str, amount: i64) {
    metrics::counter!("accounting_settlement_success_total", "provider" => provider)
        .increment(1);
    if amount > 0 {
        metrics::counter!("accounting_settlement_au_total", "provider" => provider)
            .increment(amount as u64);
    }
}

/// Record that the trust ramp grew the outbound credit limit for a peer.
pub fn record_trust_ramp_growth(new_limit: u64) {
    metrics::counter!("accounting_trust_ramp_growth_total").increment(1);
    metrics::gauge!("accounting_trust_ramp_latest_limit_au").set(new_limit as f64);
}

/// Record that a remote credit limit was received from a peer (in AU).
pub fn record_remote_credit_limit(limit: u64) {
    metrics::counter!("accounting_credit_limit_received_total").increment(1);
    metrics::gauge!("accounting_credit_limit_received_latest_au").set(limit as f64);
}

/// Update the tracked peer count gauge.
pub fn set_peer_count(count: usize) {
    metrics::gauge!("accounting_peers").set(count as f64);
}

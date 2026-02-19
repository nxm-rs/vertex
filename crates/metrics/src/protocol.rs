//! Unified protocol stream tracking via RAII guard.

use metrics::{counter, gauge};

use crate::GaugeGuard;

/// Tracks an active protocol stream via the unified `protocol_streams_active` gauge
/// and `protocol_streams_total` counter.
///
/// On creation: increments the gauge and the counter.
/// On drop: decrements the gauge.
pub struct StreamGuard {
    _active: GaugeGuard,
}

impl StreamGuard {
    /// Create with explicit protocol and direction labels.
    pub fn new(protocol: &'static str, direction: &'static str) -> Self {
        counter!(
            "protocol_streams_total",
            "protocol" => protocol,
            "direction" => direction,
        )
        .increment(1);

        Self {
            _active: GaugeGuard::increment(gauge!(
                "protocol_streams_active",
                "protocol" => protocol,
                "direction" => direction,
            )),
        }
    }

    /// Inbound stream guard.
    pub fn inbound(protocol: &'static str) -> Self {
        Self::new(protocol, crate::labels::direction::INBOUND)
    }

    /// Outbound stream guard.
    pub fn outbound(protocol: &'static str) -> Self {
        Self::new(protocol, crate::labels::direction::OUTBOUND)
    }
}

impl core::fmt::Debug for StreamGuard {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StreamGuard").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_guard_creation() {
        let _guard = StreamGuard::new("hive", "inbound");
        let _guard = StreamGuard::inbound("identify");
        let _guard = StreamGuard::outbound("identify");
    }
}

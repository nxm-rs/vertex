//! No-op settlement provider for `BandwidthMode::None`.

use vertex_swarm_api::{BandwidthMode, SwarmPeerAccounting, SwarmResult, SwarmSettlementProvider};
use vertex_swarm_primitives::OverlayAddress;

/// No-op settlement provider (always returns zero, never settles).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSettlement;

#[async_trait::async_trait]
impl SwarmSettlementProvider for NoSettlement {
    fn supported_mode(&self) -> BandwidthMode {
        BandwidthMode::None
    }

    fn pre_allow(&self, _peer: OverlayAddress, _state: &dyn SwarmPeerAccounting) -> i64 {
        0i64
    }

    async fn settle(&self, _peer: OverlayAddress, _state: &dyn SwarmPeerAccounting) -> SwarmResult<i64> {
        Ok(0i64)
    }

    fn name(&self) -> &'static str {
        "none"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PeerAccounting;
    use vertex_swarm_test_utils::test_peer;

    #[test]
    fn test_no_settlement_provider() {
        let provider = NoSettlement;
        let state = PeerAccounting::new(13_500_000, 16_875_000);

        assert_eq!(provider.pre_allow(test_peer(), &state), 0);
        assert_eq!(provider.name(), "none");
        assert_eq!(provider.supported_mode(), BandwidthMode::None);
    }
}

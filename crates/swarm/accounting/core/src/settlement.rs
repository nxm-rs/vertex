//! No-op settlement provider.

use vertex_swarm_api::{Au, SwarmPeerState, SwarmResult, SwarmSettlementProvider};
use vertex_swarm_primitives::OverlayAddress;

/// No-op settlement provider (always returns 0, never settles).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSettlement;

#[async_trait::async_trait]
impl SwarmSettlementProvider for NoSettlement {
    fn pre_allow(&self, _peer: OverlayAddress, _state: &dyn SwarmPeerState) -> Au {
        Au::ZERO
    }

    async fn settle(&self, _peer: OverlayAddress, _state: &dyn SwarmPeerState) -> SwarmResult<Au> {
        Ok(Au::ZERO)
    }

    fn name(&self) -> &'static str {
        "none"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PeerState;
    use vertex_swarm_test_utils::test_peer;

    #[test]
    fn test_no_settlement_provider() {
        let provider = NoSettlement;
        let state = PeerState::new(Au::from_amount(13_500_000), Au::from_amount(16_875_000));

        assert_eq!(provider.pre_allow(test_peer(), &state), Au::ZERO);
        assert_eq!(provider.name(), "none");
    }
}

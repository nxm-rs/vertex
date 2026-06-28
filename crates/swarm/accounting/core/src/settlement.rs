//! No-op settlement provider.

use vertex_swarm_api::{Au, SwarmPeerState, SwarmResult, SwarmSettlementProvider};
use vertex_swarm_primitives::OverlayAddress;

/// No-op settlement provider (always returns 0, never settles).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSettlement;

#[async_trait::async_trait]
impl SwarmSettlementProvider for NoSettlement {
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

    #[test]
    fn test_no_settlement_provider() {
        let provider = NoSettlement;
        assert_eq!(provider.name(), "none");
    }
}

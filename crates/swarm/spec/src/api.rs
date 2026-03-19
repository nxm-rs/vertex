//! SwarmSpec trait implementation for Spec.

use crate::{
    Spec, Token,
    constants::{mainnet, testnet},
};
use alloc::{string::String, vec::Vec};
use alloy_chains::Chain;
use nectar_primitives::StandardChunkSet;
use nectar_swarms::Swarm;
use vertex_swarm_api::{SwarmSpec, SwarmSpecProvider};
use vertex_swarm_forks::{ForkCondition, ForkDigest, SwarmHardfork, SwarmHardforks};

impl SwarmSpec for Spec {
    type ChunkSet = StandardChunkSet;
    type Token = Token;

    fn swarm(&self) -> Swarm {
        match self.network_id {
            mainnet::NETWORK_ID => nectar_swarms::NamedSwarm::Mainnet.into(),
            testnet::NETWORK_ID => nectar_swarms::NamedSwarm::Testnet.into(),
            _ => Swarm::from_id(self.network_id),
        }
    }

    fn chain(&self) -> Chain {
        self.chain
    }

    fn network_id(&self) -> u64 {
        self.network_id
    }

    fn network_name(&self) -> &str {
        &self.network_name
    }

    fn bootnodes(&self) -> Option<Vec<String>> {
        if self.bootnodes.is_empty() {
            None
        } else {
            Some(self.bootnodes.clone())
        }
    }

    fn token(&self) -> &Token {
        &self.token
    }

    fn hardforks(&self) -> &SwarmHardforks {
        &self.hardforks
    }

    fn reserve_capacity(&self) -> u64 {
        self.reserve_capacity
    }

    fn is_fork_active_at_timestamp(&self, fork: SwarmHardfork, timestamp: u64) -> bool {
        match self.hardforks.get(fork) {
            Some(ForkCondition::Timestamp(activation_time)) => timestamp >= activation_time,
            _ => false,
        }
    }

    fn fork_digest(&self, at_timestamp: u64) -> ForkDigest {
        // Collect active fork timestamps
        let active_forks: Vec<u64> = self
            .hardforks
            .forks_iter()
            .filter_map(|(_, condition)| {
                if let ForkCondition::Timestamp(activation) = condition
                    && activation <= at_timestamp
                {
                    return Some(activation);
                }
                None
            })
            .collect();

        ForkDigest::compute(self.network_id, self.genesis_timestamp, &active_forks)
    }
}

impl SwarmSpecProvider for Spec {
    type Spec = Spec;

    fn spec(&self) -> &Self::Spec {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SpecBuilder, init_mainnet, init_testnet};
    use vertex_swarm_api::StaticSwarmSpecProvider;

    #[test]
    fn test_swarm_spec_trait() {
        // Verify Spec implements SwarmSpec
        fn assert_spec<S: SwarmSpec>(_s: &S) {}

        let mainnet = init_mainnet();
        assert_spec(&*mainnet);

        let testnet = init_testnet();
        assert_spec(&*testnet);
    }

    #[test]
    fn test_network_checks() {
        let mainnet = init_mainnet();
        assert!(mainnet.is_mainnet());
        assert!(!mainnet.is_testnet());
        assert!(!mainnet.is_dev());

        let testnet = init_testnet();
        assert!(!testnet.is_mainnet());
        assert!(testnet.is_testnet());
        assert!(!testnet.is_dev());

        let dev = SpecBuilder::dev().build();
        assert!(!dev.is_mainnet());
        assert!(!dev.is_testnet());
        assert!(dev.is_dev());
    }

    #[test]
    fn test_spec_provider() {
        let spec = init_mainnet();
        let provider: StaticSwarmSpecProvider<Spec> =
            StaticSwarmSpecProvider::from_arc(spec.clone());

        assert_eq!(provider.spec().network_id(), spec.network_id());
    }

    #[test]
    fn test_hardforks() {
        let spec = init_mainnet();
        let hardforks = spec.hardforks();

        // Test that we can query hardforks
        let genesis = hardforks.get(SwarmHardfork::Genesis);
        assert!(matches!(genesis, Some(ForkCondition::Timestamp(_))));
    }

    #[test]
    fn test_fork_digest() {
        let mainnet = init_mainnet();
        let testnet = init_testnet();

        // Same network at same time should produce same digest
        let digest1 = mainnet.fork_digest(1000000);
        let digest2 = mainnet.fork_digest(1000000);
        assert_eq!(digest1, digest2);

        // Different networks should produce different digests
        let mainnet_digest = mainnet.fork_digest(1000000);
        let testnet_digest = testnet.fork_digest(1000000);
        assert_ne!(mainnet_digest, testnet_digest);

        // Digest display works
        let digest = mainnet.fork_digest(1000000);
        let display = alloc::format!("{}", digest);
        assert!(display.starts_with("0x"));
        assert_eq!(display.len(), 10); // "0x" + 8 hex chars
    }

    #[test]
    fn test_fork_digest_changes_with_active_forks() {
        // Build two specs with different fork activation times
        let spec1 = SpecBuilder::new()
            .network_id(100)
            .with_accord(1000)
            .genesis_timestamp(0)
            .build();

        let spec2 = SpecBuilder::new()
            .network_id(100)
            .with_accord(2000)
            .genesis_timestamp(0)
            .build();

        // Before any fork is active, digests should differ only by fork timestamps in hash
        // At timestamp 500, neither fork is active
        let d1_before = spec1.fork_digest(500);
        let d2_before = spec2.fork_digest(500);
        assert_eq!(d1_before, d2_before); // Same because no forks active

        // At timestamp 1500, spec1's accord is active but not spec2's
        let d1_after = spec1.fork_digest(1500);
        let d2_after = spec2.fork_digest(1500);
        assert_ne!(d1_after, d2_after); // Different because different forks active
    }

    #[test]
    fn test_next_fork_timestamp() {
        // Build a spec with a future fork
        let spec = SpecBuilder::new()
            .network_id(100)
            .with_accord(1000)
            .genesis_timestamp(0)
            .build();

        // Before the fork, next_fork_timestamp should return the fork time
        assert_eq!(spec.next_fork_timestamp(0), Some(1000));
        assert_eq!(spec.next_fork_timestamp(500), Some(1000));

        // After/at the fork, no more forks
        assert_eq!(spec.next_fork_timestamp(1000), None);
        assert_eq!(spec.next_fork_timestamp(2000), None);
    }
}

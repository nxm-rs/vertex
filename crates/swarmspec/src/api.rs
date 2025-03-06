//! API for interacting with Swarm network specifications

use crate::{LightClient, NetworkSpec, Storage, Token};
use alloc::vec::Vec;
use alloy_chains::Chain;
use core::fmt::Debug;
use libp2p::Multiaddr;
use vertex_network_primitives::Swarm;
use vertex_swarm_forks::{ForkCondition, SwarmHardfork};

/// Trait representing type configuring a Swarm network specification
#[auto_impl::auto_impl(&, Arc)]
pub trait SwarmSpec: Send + Sync + Unpin + Debug {
    /// Returns the corresponding Swarm network
    fn swarm(&self) -> Swarm;

    /// Returns the [`Chain`] object this spec targets
    fn chain(&self) -> Chain;

    /// Returns the network ID for the Swarm network
    fn network_id(&self) -> u64;

    /// Returns the Swarm network name (like "mainnet", "testnet", etc.)
    fn network_name(&self) -> &str;

    /// Returns the bootnodes for the network
    fn bootnodes(&self) -> Vec<Multiaddr>;

    /// Returns the storage configuration
    fn storage(&self) -> &Storage;

    /// Returns the bandwidth incentives configuration
    fn bandwidth(&self) -> &LightClient;

    /// Returns the Swarm token details
    fn token(&self) -> &Token;

    /// Returns the fork activation status for a given Swarm hardfork at a timestamp
    fn is_fork_active_at_timestamp(&self, fork: SwarmHardfork, timestamp: u64) -> bool;

    /// Returns whether this is the mainnet Swarm
    fn is_mainnet(&self) -> bool {
        self.network_id() == 1
    }

    /// Returns whether this is a testnet Swarm
    fn is_testnet(&self) -> bool {
        self.network_id() == 10
    }
}

impl SwarmSpec for NetworkSpec {
    fn swarm(&self) -> Swarm {
        match self.network_id {
            1 => vertex_network_primitives::NamedSwarm::Mainnet.into(),
            10 => vertex_network_primitives::NamedSwarm::Testnet.into(),
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

    fn bootnodes(&self) -> Vec<Multiaddr> {
        self.bootnodes.clone()
    }

    fn storage(&self) -> &Storage {
        &self.storage
    }

    fn bandwidth(&self) -> &LightClient {
        &self.light_client
    }

    fn token(&self) -> &Token {
        &self.token
    }

    fn is_fork_active_at_timestamp(&self, fork: SwarmHardfork, timestamp: u64) -> bool {
        match self.hardforks.get(fork) {
            Some(ForkCondition::Timestamp(activation_time)) => timestamp >= activation_time,
            _ => false,
        }
    }
}

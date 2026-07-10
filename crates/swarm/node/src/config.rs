//! Swarm protocol configuration (serializable Args layer).

use std::path::Path;
use std::sync::Arc;

use eyre::Result;
use serde::{Deserialize, Serialize};
use vertex_node_api::NodeProtocolConfig;
use vertex_swarm_accounting::{AccountingArgs, DefaultAccountingConfig};
use vertex_swarm_identity::{Identity, IdentityArgs};
use vertex_swarm_localstore::{LocalStoreArgs, LocalStoreConfig};
use vertex_swarm_primitives::SwarmNodeType;
#[cfg(feature = "storer")]
use vertex_swarm_redistribution::{RedistributionArgs, StorageConfig};
use vertex_swarm_spec::Spec;

use vertex_swarm_api::{ConfigError, StampValidation};

use crate::args::{
    ChainArgs, ChainConfig, NetworkArgs, NetworkConfig, ProtocolArgs, RpcArgs, SwapArgs, SwapConfig,
};

/// Swarm protocol configuration (serializable Args layer).
///
/// Contains all Swarm-specific settings for config file serialization.
/// Used as the type parameter for `vertex_node_core::config::FullNodeConfig`.
/// Pricing is nested under `accounting`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProtocolConfig {
    pub node_type: SwarmNodeType,
    pub identity: IdentityArgs,
    pub network: NetworkArgs,
    pub accounting: AccountingArgs,
    pub localstore: LocalStoreArgs,
    #[cfg(feature = "storer")]
    pub redistribution: RedistributionArgs,
    pub chain: ChainArgs,
    pub swap: SwapArgs,
    pub rpc: RpcArgs,
}

impl ProtocolConfig {
    /// Create validated network configuration.
    ///
    /// A bootnode left on the stock listen set widens it to dual-stack
    /// (IPv4 plus IPv6 wildcard listeners); explicit configuration wins.
    pub fn network_config(&self) -> Result<NetworkConfig, ConfigError> {
        let mut config = NetworkConfig::try_from(&self.network)?;
        if matches!(self.node_type, SwarmNodeType::Bootnode) {
            config.apply_dual_stack_listen_default();
        }
        Ok(config)
    }

    /// Create identity from keystore or ephemeral.
    pub fn identity(&self, spec: Arc<Spec>, network_dir: &Path) -> Result<Arc<Identity>> {
        self.identity.identity(spec, network_dir, self.node_type)
    }

    /// Build the accounting configuration.
    pub fn accounting_config(&self) -> DefaultAccountingConfig {
        DefaultAccountingConfig::from(&self.accounting)
    }

    /// Create local store configuration.
    pub fn local_store_config(&self) -> LocalStoreConfig {
        self.localstore.local_store_config()
    }

    /// Create storage incentives configuration.
    #[cfg(feature = "storer")]
    pub fn storage_config(&self) -> StorageConfig {
        self.redistribution.storage_config()
    }

    /// Create the validated chain configuration (RPC endpoint and tx tuning).
    pub fn chain_config(&self) -> ChainConfig {
        self.chain.chain_config()
    }

    /// Create the validated SWAP configuration (chequebook, beneficiary, deploy).
    pub fn swap_config(&self) -> SwapConfig {
        self.swap.swap_config()
    }

    /// Stamp-validation policy for the gRPC chunk service.
    pub fn rpc_stamp_validation(&self) -> StampValidation {
        self.rpc.stamp_validation.into()
    }
}

impl ProtocolConfig {
    /// Override the node type with the CLI-selected value, taking precedence
    /// over whatever the loaded config file carried.
    pub fn override_node_type(&mut self, node_type: SwarmNodeType) {
        self.node_type = node_type;
    }
}

impl NodeProtocolConfig for ProtocolConfig {
    type Args = ProtocolArgs;

    fn apply_args(&mut self, args: &Self::Args) {
        self.identity = args.identity.clone();
        self.network = args.network.clone();
        self.accounting = args.accounting.clone();
        self.localstore = args.localstore.clone();
        #[cfg(feature = "storer")]
        {
            self.redistribution = args.redistribution.clone();
        }
        self.chain = args.chain.clone();
        self.swap = args.swap.clone();
        self.rpc = args.rpc;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_swarm_api::{Multiaddr, SwarmNetworkConfig};

    #[test]
    fn bootnode_defaults_to_dual_stack_listeners() {
        let mut config = ProtocolConfig::default();
        config.override_node_type(SwarmNodeType::Bootnode);

        let network = config.network_config().expect("default config is valid");
        let v6: Multiaddr = "/ip6/::/tcp/1634".parse().expect("valid multiaddr");
        let v6_quic: Multiaddr = "/ip6/::/udp/1634/quic-v1".parse().expect("valid multiaddr");
        assert_eq!(network.listen_addrs().len(), 4);
        assert!(network.listen_addrs().contains(&v6));
        assert!(network.listen_addrs().contains(&v6_quic));
    }

    #[test]
    fn client_keeps_the_ipv4_listener_pair() {
        let config = ProtocolConfig::default();
        let network = config.network_config().expect("default config is valid");
        assert_eq!(network.listen_addrs().len(), 2);
    }

    #[test]
    fn explicit_listen_addrs_override_the_bootnode_default() {
        let mut config = ProtocolConfig::default();
        config.override_node_type(SwarmNodeType::Bootnode);
        config.network.listen_addrs_raw = vec!["/ip4/127.0.0.1/tcp/1700".to_string()];

        let network = config.network_config().expect("valid config");
        assert_eq!(network.listen_addrs().len(), 1);
    }
}

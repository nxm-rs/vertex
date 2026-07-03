//! Node-type-specific validated configurations holding runtime objects.
//!
//! These structs are the final tier of the configuration architecture: fully
//! assembled configs that hold runtime objects such as `Arc<Identity>` and
//! `Arc<Spec>`. They live in `vertex-swarm-builder` (not `vertex-node-core`)
//! because they depend on swarm-specific types like `Identity`,
//! `KademliaConfig`, and `Spec`.
//!
//! Each struct implements `NodeBuildsProtocol` so the builder can select the
//! correct protocol stack at construction time.
//!
//! See `docs/architecture/config.md` for the full three-tier pattern.

use std::sync::Arc;

use vertex_node_api::NodeBuildsProtocol;
use vertex_storage_redb::RedbDatabase;
use vertex_swarm_accounting::DefaultAccountingConfig;
use vertex_swarm_api::SwarmLocalStore;
use vertex_swarm_identity::Identity;
use vertex_swarm_localstore::LocalStoreConfig;
use vertex_swarm_node::args::{ChainConfig, NetworkConfig, SwapConfig};
use vertex_swarm_spec::Spec;
use vertex_swarm_topology::KademliaConfig;

use crate::error::SwarmNodeError;
use crate::launch::CacheSeam;
use crate::protocol::SwarmProtocol;

/// Implement shared config getters: `spec()`, `identity()`, `network()`.
macro_rules! impl_common_config_getters {
    ($ty:ident) => {
        impl $ty {
            pub fn spec(&self) -> &Arc<Spec> {
                &self.spec
            }

            pub fn identity(&self) -> &Arc<Identity> {
                &self.identity
            }

            pub fn network(&self) -> &NetworkConfig<KademliaConfig> {
                &self.network
            }
        }
    };
}

/// Implement `NodeBuildsProtocol` with a given protocol name.
macro_rules! impl_builds_protocol {
    ($ty:ident, $name:expr) => {
        impl NodeBuildsProtocol for $ty {
            type Protocol = SwarmProtocol<Self>;

            fn protocol_name(&self) -> &'static str {
                $name
            }
        }
    };
}

/// Validated configuration for bootnode (network identity and topology only).
#[derive(Clone)]
pub struct BootnodeConfig {
    spec: Arc<Spec>,
    identity: Arc<Identity>,
    network: NetworkConfig<KademliaConfig>,
}

impl BootnodeConfig {
    pub fn new(
        spec: Arc<Spec>,
        identity: Arc<Identity>,
        network: NetworkConfig<KademliaConfig>,
    ) -> Self {
        Self {
            spec,
            identity,
            network,
        }
    }
}

impl_common_config_getters!(BootnodeConfig);
impl_builds_protocol!(BootnodeConfig, "Swarm Bootnode");

/// Validated configuration for a Client node with accounting.
pub struct ClientConfig {
    spec: Arc<Spec>,
    identity: Arc<Identity>,
    network: NetworkConfig<KademliaConfig>,
    accounting: DefaultAccountingConfig,
    local_store: LocalStoreConfig,
    chain: ChainConfig,
    swap: SwapConfig,
    cache: Option<CacheSeam>,
}

impl ClientConfig {
    pub fn new(
        spec: Arc<Spec>,
        identity: Arc<Identity>,
        network: NetworkConfig<KademliaConfig>,
        accounting: DefaultAccountingConfig,
        local_store: LocalStoreConfig,
        chain: ChainConfig,
        swap: SwapConfig,
    ) -> Self {
        Self {
            spec,
            identity,
            network,
            accounting,
            local_store,
            chain,
            swap,
            cache: None,
        }
    }

    /// Override the cache with a pre-built local store, replacing the default
    /// in-memory chunk store.
    ///
    /// On a client this is the full retrieval-serve view; on a storer only the
    /// forwarding-cache layer of the cache-then-reserve view (read after the
    /// reserve, written for out-of-AoR chunks, never for reserve admission).
    pub fn with_cache(mut self, cache: Arc<dyn SwarmLocalStore>) -> Self {
        self.cache = Some(CacheSeam::Ready(cache));
        self
    }

    /// Override the cache with a factory invoked at build time.
    ///
    /// The factory receives the opened shared database (`None` in-memory) so the
    /// cache can size or back itself from the same handle. The client-versus-storer
    /// seam meaning of [`with_cache`](Self::with_cache) applies.
    pub fn with_cache_factory<F>(mut self, factory: F) -> Self
    where
        F: FnOnce(Option<Arc<RedbDatabase>>) -> Result<Arc<dyn SwarmLocalStore>, SwarmNodeError>
            + Send
            + Sync
            + 'static,
    {
        self.cache = Some(CacheSeam::Factory(Box::new(factory)));
        self
    }

    pub(crate) fn take_cache(&mut self) -> Option<CacheSeam> {
        self.cache.take()
    }

    pub fn accounting(&self) -> &DefaultAccountingConfig {
        &self.accounting
    }

    /// Cache sizing for the client's in-memory local store.
    pub fn local_store(&self) -> &LocalStoreConfig {
        &self.local_store
    }

    pub fn chain(&self) -> &ChainConfig {
        &self.chain
    }

    /// SWAP settlement configuration (chequebook, beneficiary, deploy).
    pub fn swap(&self) -> &SwapConfig {
        &self.swap
    }
}

impl_common_config_getters!(ClientConfig);
impl_builds_protocol!(ClientConfig, "Swarm Client");

#[cfg(test)]
mod tests {
    use vertex_swarm_api::SwarmNetworkConfig;

    use super::NetworkConfig;

    /// The binary stamps `vertex-node-core`'s build-stamped agent string onto the
    /// network config the launch path reads; this asserts that exact value carries
    /// the git sha and round-trips through the config the node assembles from.
    #[test]
    fn stamped_network_config_announces_the_build_sha() {
        let agent = vertex_node_core::version::AGENT_VERSION.clone();
        let network = NetworkConfig::default().with_agent_version(agent.clone());

        assert_eq!(network.agent_version(), Some(agent.as_str()));
        assert!(agent.starts_with("vertex/"));
        assert_ne!(
            vertex_node_core::version::GIT_SHA,
            "unknown",
            "build.rs did not stamp the git sha"
        );
        assert!(
            agent.contains(vertex_node_core::version::GIT_SHA),
            "agent string {agent} is missing the build sha"
        );
    }
}

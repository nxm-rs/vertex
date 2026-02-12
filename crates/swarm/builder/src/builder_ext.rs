//! Shared traits for fluent builder APIs.

use std::path::PathBuf;

use vertex_node_api::InfrastructureContext;
use vertex_swarm_api::{PeerConfigValues, SwarmPeerConfig};
use vertex_swarm_spec::Loggable;

/// Fluent transformation API for builders.
pub trait BuilderExt: Sized {
    fn apply<F>(self, f: F) -> Self
    where
        F: FnOnce(Self) -> Self,
    {
        f(self)
    }

    fn apply_if<F>(self, cond: bool, f: F) -> Self
    where
        F: FnOnce(Self) -> Self,
    {
        if cond {
            f(self)
        } else {
            self
        }
    }
}

/// Infrastructure integration for builders with network configuration.
pub trait WithInfrastructure<N: SetPeerStorePath>: Sized {
    fn network_mut(&mut self) -> &mut N;

    /// Apply infrastructure defaults (peer store path) from launch context.
    fn with_infrastructure(mut self, ctx: &dyn InfrastructureContext) -> Self {
        let path = ctx.data_dir().join("state").join("peers.json");
        self.network_mut().set_default_peer_store_path(path);
        self
    }
}

/// Set default peer store path on network configuration.
pub trait SetPeerStorePath {
    fn set_default_peer_store_path(&mut self, path: PathBuf);
}

impl<R: Default> SetPeerStorePath for vertex_swarm_node::args::NetworkConfig<R> {
    fn set_default_peer_store_path(&mut self, path: PathBuf) {
        vertex_swarm_node::args::NetworkConfig::set_default_peer_store_path(self, path);
    }
}

/// Log build preamble: node type, spec info, peers path.
pub(crate) fn log_build_start<N>(node_type: &str, spec: &vertex_swarm_spec::Spec, network: &N)
where
    N: SwarmPeerConfig,
    N::Peers: PeerConfigValues,
{
    use tracing::info;

    info!("Building {} node...", node_type);
    spec.log();

    match network.peers().store_path() {
        Some(path) => info!(path = %path.display(), "Peers database"),
        None => info!("Peers database: ephemeral"),
    }
}

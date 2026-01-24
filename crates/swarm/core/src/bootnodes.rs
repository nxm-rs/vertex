//! Bootnode provider for resolving bootstrap addresses.
//!
//! This module bridges the abstract `SwarmSpec` (which stores bootnodes as strings)
//! with the networking layer (which needs `Multiaddr`).

use libp2p::Multiaddr;
use vertex_swarmspec::SwarmSpec;

/// Provides bootstrap node addresses for network discovery.
///
/// This trait exists to bridge `SwarmSpec` (which stores bootnodes as strings
/// to avoid libp2p dependencies) with the networking layer.
pub trait BootnodeProvider {
    /// Returns the bootstrap node addresses.
    ///
    /// Invalid multiaddr strings are silently filtered out.
    fn bootnodes(&self) -> Vec<Multiaddr>;
}

impl<S: SwarmSpec> BootnodeProvider for S {
    fn bootnodes(&self) -> Vec<Multiaddr> {
        SwarmSpec::bootnodes(self)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|s| s.parse().ok())
            .collect()
    }
}

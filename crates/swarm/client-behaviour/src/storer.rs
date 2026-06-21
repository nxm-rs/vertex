//! Storer ingest capability for the pushsync inbound path.
//!
//! Optional capability that lets the inbound handler take custody of a delivery
//! it is responsible for: put the chunk into the reserve and sign its own
//! receipt instead of relaying. When absent, the handler runs the client path
//! (forward to a closer peer, relay the receipt). Arc-cheap to clone into each
//! connection handler.

use std::sync::Arc;

use vertex_swarm_api::ReserveStore;
use vertex_swarm_primitives::OverlaySigner;

/// Reserve plus the node's overlay-signing identity, shared into each handler.
///
/// The identity is erased to `Arc<dyn OverlaySigner>` so the non-generic client
/// behaviour can hold it; the handler mints a custody receipt straight from it
/// via [`vertex_swarm_net_pushsync::Receipt::sign`], which reads the signer and
/// the overlay-derivation inputs (`network_id`, `nonce`) through the one handle.
/// `put` admits unconditionally; capacity is held out of band by the eviction
/// primitives ([`ReserveStore::evict_from_bin`] /
/// [`evict_batch`](ReserveStore::evict_batch)), not by ingest back-pressure.
#[derive(Clone)]
pub struct StorerCapability {
    pub(crate) reserve: Arc<dyn ReserveStore>,
    pub(crate) signer: Arc<dyn OverlaySigner + Send + Sync>,
}

impl StorerCapability {
    pub fn new(
        reserve: Arc<dyn ReserveStore>,
        signer: Arc<dyn OverlaySigner + Send + Sync>,
    ) -> Self {
        Self { reserve, signer }
    }
}

impl std::fmt::Debug for StorerCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never log the signing key; show only the public overlay-derivation inputs.
        f.debug_struct("StorerCapability")
            .field("network_id", &self.signer.network_id())
            .field("nonce", &self.signer.nonce())
            .finish_non_exhaustive()
    }
}

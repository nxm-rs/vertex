//! Storer ingest capability for the pushsync inbound path.
//!
//! Optional capability that lets the inbound handler take custody of a delivery
//! it is responsible for: put the chunk into the reserve and sign its own
//! receipt instead of relaying. When absent, the handler runs the client path
//! (forward to a closer peer, relay the receipt). Arc-cheap to clone into each
//! connection handler.

use std::sync::Arc;

use alloy_signer::SignerSync;
use vertex_swarm_api::ReserveStore;
use vertex_swarm_net_pushsync::ReceiptSigner;
use vertex_swarm_primitives::{NetworkId, Nonce};

/// Reserve plus the node's receipt-signing identity, shared into each handler.
///
/// `network_id` and `nonce` are the overlay-derivation inputs a forwarder
/// recovers from the minted receipt; implementing [`ReceiptSigner`] lets the
/// handler pass this straight to [`vertex_swarm_net_pushsync::Receipt::sign`].
/// `put` admits unconditionally; capacity is held out of band by the eviction
/// primitives ([`ReserveStore::evict_from_bin`] /
/// [`evict_batch`](ReserveStore::evict_batch)), not by ingest back-pressure.
#[derive(Clone)]
pub struct StorerCapability {
    pub(crate) reserve: Arc<dyn ReserveStore>,
    signer: Arc<dyn SignerSync + Send + Sync>,
    network_id: NetworkId,
    nonce: Nonce,
}

impl StorerCapability {
    pub fn new(
        reserve: Arc<dyn ReserveStore>,
        signer: Arc<dyn SignerSync + Send + Sync>,
        network_id: NetworkId,
        nonce: Nonce,
    ) -> Self {
        Self {
            reserve,
            signer,
            network_id,
            nonce,
        }
    }
}

impl ReceiptSigner for StorerCapability {
    type Signer = dyn SignerSync + Send + Sync;

    fn signer(&self) -> &Self::Signer {
        &*self.signer
    }

    fn network_id(&self) -> NetworkId {
        self.network_id
    }

    fn nonce(&self) -> Nonce {
        self.nonce
    }
}

impl std::fmt::Debug for StorerCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never log the signer key; show only the public overlay-derivation inputs.
        f.debug_struct("StorerCapability")
            .field("network_id", &self.network_id)
            .field("nonce", &self.nonce)
            .finish_non_exhaustive()
    }
}

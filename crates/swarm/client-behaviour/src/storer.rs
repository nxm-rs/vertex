//! Storer ingest capability for the pushsync inbound path.
//!
//! Optional capability that lets the inbound handler take custody of a delivery
//! it is responsible for: put the chunk into the reserve and sign its own
//! receipt instead of relaying. When absent, the handler runs the client path
//! (forward to a closer peer, relay the receipt). Arc-cheap to clone into each
//! connection handler.

use std::sync::Arc;

use alloy_primitives::{Address, B256, ChainId, Signature};
use vertex_swarm_api::{ReserveStore, SwarmIdentity, SwarmSpec};
use vertex_swarm_primitives::{NetworkId, Nonce, OverlaySigner, SignerSync};

/// Reserve plus the node's receipt-signing identity, shared into each handler.
///
/// The signer is erased to `dyn SignerSync`; `address`, `network_id` and `nonce`
/// are the overlay-derivation inputs a forwarder recovers from the minted
/// receipt. Implementing [`OverlaySigner`] lets the handler pass this straight to
/// [`vertex_swarm_net_pushsync::Receipt::sign`]. `put` admits unconditionally;
/// capacity is held out of band by the eviction primitives
/// ([`ReserveStore::evict_from_bin`] / [`evict_batch`](ReserveStore::evict_batch)),
/// not by ingest back-pressure.
#[derive(Clone)]
pub struct StorerCapability {
    pub(crate) reserve: Arc<dyn ReserveStore>,
    signer: Arc<dyn SignerSync + Send + Sync>,
    // `dyn SignerSync` omits `address()` (it is on the async `Signer`), so the
    // overlay-derivation address is captured at construction.
    address: Address,
    network_id: NetworkId,
    nonce: Nonce,
}

impl StorerCapability {
    /// Build the capability from the node identity. The signer is erased to
    /// `dyn SignerSync`; `address`, `network_id` and `nonce` are read off the
    /// identity so the minted receipt's overlay derives from the same key.
    pub fn new(reserve: Arc<dyn ReserveStore>, identity: &impl SwarmIdentity) -> Self {
        // `I::Signer: Signer + SignerSync + Send + Sync`, so erasing to
        // SignerSync is sound.
        let signer: Arc<dyn SignerSync + Send + Sync> = identity.signer();
        Self {
            reserve,
            signer,
            address: identity.address(),
            network_id: identity.spec().network_id(),
            nonce: identity.nonce(),
        }
    }
}

impl SignerSync for StorerCapability {
    fn sign_hash_sync(&self, hash: &B256) -> alloy_signer::Result<Signature> {
        self.signer.sign_hash_sync(hash)
    }

    fn chain_id_sync(&self) -> Option<ChainId> {
        self.signer.chain_id_sync()
    }
}

impl OverlaySigner for StorerCapability {
    fn address(&self) -> Address {
        self.address
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
